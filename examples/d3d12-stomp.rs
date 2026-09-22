//! Stomp mode demo. See `docs/d3d12-stomp-allocator.md` and [`StompSettings`].
//!
//! Creates a stomp-guarded buffer named "stomp me" and writes to its tail guard page through
//! a root UAV. Root descriptors are not bounds checked, which is exactly the kind of access
//! stomp mode exists to catch. The GPU page-faults and the device is removed. Exit code 0
//! when that happened, 1 otherwise.
//!
//! DRED page fault attribution is printed when the driver provides it; NVIDIA currently
//! reports the removal but no fault VA. WARP does not page-fault, so on WARP the demo only
//! prints the placement.
use gpu_allocator::{
    d3d12::{
        Allocator, AllocatorCreateDesc, ID3D12DeviceVersion, ResourceCategory, ResourceCreateDesc,
        ResourceStateOrBarrierLayout, ResourceType, StompSettings,
    },
    MemoryLocation,
};
use windows::{
    core::{Interface, Result},
    Win32::{
        Foundation::HANDLE,
        Graphics::{
            Direct3D::D3D_FEATURE_LEVEL_11_0,
            Direct3D12::*,
            Dxgi::{
                Common::{DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC},
                CreateDXGIFactory2, IDXGIAdapter1, IDXGIFactory6, DXGI_ADAPTER_FLAG_SOFTWARE,
                DXGI_ERROR_NOT_FOUND,
            },
        },
    },
};

/// Compute shader that stores 4 bytes through a root UAV bound at an arbitrary GPU VA.
/// Source and compile command in `tests/shaders/`.
const ROOT_UAV_WRITE_CS: &[u8] = include_bytes!("../tests/shaders/root_uav_write.dxil");

fn enable_dred() {
    let mut settings: Option<ID3D12DeviceRemovedExtendedDataSettings> = None;
    match unsafe { D3D12GetDebugInterface(&mut settings) } {
        Ok(()) => unsafe {
            let settings = settings.unwrap();
            settings.SetPageFaultEnablement(D3D12_DRED_ENABLEMENT_FORCED_ON);
            settings.SetAutoBreadcrumbsEnablement(D3D12_DRED_ENABLEMENT_FORCED_ON);
        },
        Err(e) => println!("DRED unavailable ({e}), the fault will not be attributed"),
    }
}

/// Hardware adapter first, WARP as fallback. Returns `(device, adapter name, is_warp)`.
fn create_device() -> Result<(ID3D12Device, String, bool)> {
    let factory: IDXGIFactory6 = unsafe { CreateDXGIFactory2(Default::default()) }?;
    for idx in 0.. {
        let adapter: IDXGIAdapter1 = match unsafe { factory.EnumAdapters1(idx) } {
            Ok(a) => a,
            Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(e) => return Err(e),
        };
        let desc = unsafe { adapter.GetDesc1() }?;
        if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
            continue;
        }
        let mut device: Option<ID3D12Device> = None;
        if unsafe { D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_11_0, &mut device) }.is_ok() {
            let name = String::from_utf16_lossy(&desc.Description)
                .trim_end_matches('\0')
                .to_string();
            return Ok((device.unwrap(), name, false));
        }
    }
    let warp: IDXGIAdapter1 = unsafe { factory.EnumWarpAdapter() }?;
    let mut device: Option<ID3D12Device> = None;
    unsafe { D3D12CreateDevice(&warp, D3D_FEATURE_LEVEL_11_0, &mut device) }?;
    Ok((device.unwrap(), "WARP".into(), true))
}

fn create_queue(device: &ID3D12Device, ty: D3D12_COMMAND_LIST_TYPE) -> Result<ID3D12CommandQueue> {
    unsafe {
        device.CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
            Type: ty,
            ..Default::default()
        })
    }
}

/// Runs one dispatch of the root-UAV-write kernel at `va` on a direct queue and waits.
fn write_at(device: &ID3D12Device, va: u64) -> Result<()> {
    unsafe {
        let root_signature: ID3D12RootSignature =
            device.CreateRootSignature(0, ROOT_UAV_WRITE_CS)?;
        let pso: ID3D12PipelineState =
            device.CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                pRootSignature: std::mem::ManuallyDrop::new(Some(root_signature.clone())),
                CS: D3D12_SHADER_BYTECODE {
                    pShaderBytecode: ROOT_UAV_WRITE_CS.as_ptr().cast(),
                    BytecodeLength: ROOT_UAV_WRITE_CS.len(),
                },
                ..Default::default()
            })?;
        let queue = create_queue(device, D3D12_COMMAND_LIST_TYPE_DIRECT)?;
        let allocator: ID3D12CommandAllocator =
            device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)?;
        let list: ID3D12GraphicsCommandList =
            device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None)?;
        list.SetComputeRootSignature(&root_signature);
        list.SetPipelineState(&pso);
        list.SetComputeRootUnorderedAccessView(0, va);
        list.Dispatch(1, 1, 1);
        list.Close()?;
        queue.ExecuteCommandLists(&[Some(list.cast()?)]);
        let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE)?;
        queue.Signal(&fence, 1)?;
        // A null event blocks the calling thread until the fence reaches the value.
        fence.SetEventOnCompletion(1, HANDLE::default())
    }
}

/// Names DRED associates with the faulting VA, when the driver attributes page faults.
fn dred_page_fault(device: &ID3D12Device) -> Option<(u64, Vec<String>)> {
    let dred: ID3D12DeviceRemovedExtendedData = device.cast().ok()?;
    let out = unsafe { dred.GetPageFaultAllocationOutput() }.ok()?;
    let mut names = Vec::new();
    for head in [
        out.pHeadExistingAllocationNode,
        out.pHeadRecentFreedAllocationNode,
    ] {
        let mut node = head;
        while !node.is_null() {
            let n = unsafe { &*node };
            if !n.ObjectNameW.is_null() {
                names.push(unsafe { n.ObjectNameW.to_string() }.unwrap_or_default());
            }
            node = n.pNext;
        }
    }
    Some((out.PageFaultVA, names))
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    enable_dred();
    let (device, name, is_warp) = create_device()?;
    println!("adapter: {name}");

    // Tile mappings are enqueued on this queue and CPU-waited, so keep it off the main queue.
    let mapping_queue = create_queue(&device, D3D12_COMMAND_LIST_TYPE_COPY)?;
    let mut allocator = Allocator::new(&AllocatorCreateDesc {
        device: ID3D12DeviceVersion::Device(device.clone()),
        debug_settings: Default::default(),
        allocation_sizes: Default::default(),
        stomp: Some(StompSettings::new(mapping_queue)),
    })
    .expect("Allocator::new");

    let desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
        Alignment: 0,
        Width: 4000,
        Height: 1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: DXGI_FORMAT_UNKNOWN,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        Flags: D3D12_RESOURCE_FLAG_NONE,
    };
    let resource = allocator
        .create_resource(&ResourceCreateDesc {
            name: "stomp me",
            memory_location: MemoryLocation::GpuOnly,
            resource_category: ResourceCategory::Buffer,
            resource_desc: &desc,
            castable_formats: &[],
            clear_value: None,
            initial_state_or_layout: ResourceStateOrBarrierLayout::ResourceState(
                D3D12_RESOURCE_STATE_COMMON,
            ),
            resource_type: &ResourceType::Placed,
            stomp: None,
        })
        .expect("create_resource");

    let layout = resource.stomp_layout().expect("stomp path");
    println!("is_stomp_guarded: {}", resource.is_stomp_guarded());
    println!("stomp_layout: {layout:#x?}");
    println!("stomp_statistics: {:?}", allocator.stomp_statistics());

    if is_warp {
        println!("WARP does not page-fault on never-resident memory, stopping here");
        allocator.free_resource(resource).unwrap();
        std::process::exit(1);
    }

    let guard_va = layout.tail_guard_va().expect("tail guard");
    println!("writing 4 bytes at the tail guard {guard_va:#x}");
    if let Err(e) = write_at(&device, guard_va) {
        println!("submission error: {e}");
    }

    let removed = match unsafe { device.GetDeviceRemovedReason() } {
        Ok(()) => {
            println!("device NOT removed: the guard access went unnoticed");
            false
        }
        Err(reason) => {
            println!("device removed: {reason}");
            // Fault data is gathered asynchronously by the runtime; give it a moment.
            std::thread::sleep(std::time::Duration::from_millis(500));
            match dred_page_fault(&device) {
                Some((va, names)) if names.iter().any(|n| n == "stomp me") => {
                    println!("DRED page fault VA {va:#x} named \"stomp me\"")
                }
                Some((va, names)) => println!(
                    "DRED page fault VA {va:#x}, allocations {names:?}: this driver does not \
                     attribute the fault"
                ),
                None => println!("DRED page fault output unavailable"),
            }
            true
        }
    };
    // Freeing after device removal is still well-formed on the CPU side.
    allocator.free_resource(resource).unwrap();
    std::process::exit(if removed { 0 } else { 1 });
}
