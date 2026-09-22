//! Hardware-only fault tests for D3D12 stomp mode. See `docs/d3d12-stomp-allocator.md`
//! section 6.3.
//!
//! Every test removes the device, so each creates its own device and the file must run
//! single-threaded. All tests are `#[ignore]` and additionally return early unless
//! `GPU_ALLOCATOR_STOMP_FAULT_TESTS=1` is set. WARP does not page-fault, so the tests also
//! return early when no hardware adapter is present.
//!
//! ```text
//! GPU_ALLOCATOR_STOMP_FAULT_TESTS=1 cargo test --features d3d12 --test d3d12_stomp_fault -- --ignored --test-threads=1 --nocapture
//! ```
//!
//! Findings on NVIDIA (RTX 5070 Ti, September 2026): a root descriptor access to a guard
//! tile removes the device with `DXGI_ERROR_DEVICE_HUNG`, but DRED reports page fault VA 0
//! and no allocation names, so attribution is informational only. Oversized
//! `CopyBufferRegion` is rejected by the runtime at `Close()` with `E_INVALIDARG`, so copies
//! cannot be used to overrun; the tests touch guard tiles through root UAV/SRV descriptors,
//! which the hardware does not bounds check.
//!
//! The allocator's tile mappings go through a COPY queue while every dispatch here runs on a
//! DIRECT queue, so each test also proves the CPU wait after mapping makes the resource
//! usable on another queue.
#![cfg(all(windows, feature = "d3d12"))]

use gpu_allocator::{
    d3d12::{
        Allocator, AllocatorCreateDesc, ID3D12DeviceVersion, Resource, ResourceCategory,
        ResourceCreateDesc, ResourceStateOrBarrierLayout, ResourceType, StompSettings,
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
                Common::{DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC},
                CreateDXGIFactory2, IDXGIAdapter1, IDXGIFactory6, DXGI_ADAPTER_FLAG_SOFTWARE,
                DXGI_ERROR_NOT_FOUND,
            },
        },
    },
};

const TILE: u64 = D3D12_TILED_RESOURCE_TILE_SIZE_IN_BYTES as u64;

/// Source and compile commands in `tests/shaders/`. Root signatures are embedded.
const ROOT_SRV_READ_CS: &[u8] = include_bytes!("shaders/root_srv_read.dxil");
const ROOT_UAV_WRITE_CS: &[u8] = include_bytes!("shaders/root_uav_write.dxil");
const TABLE_SRV_TEXTURE_LOAD_CS: &[u8] = include_bytes!("shaders/table_srv_texture_load.dxil");

fn enabled() -> bool {
    if std::env::var("GPU_ALLOCATOR_STOMP_FAULT_TESTS").as_deref() != Ok("1") {
        eprintln!("skipped: set GPU_ALLOCATOR_STOMP_FAULT_TESTS=1 to run fault tests");
        return false;
    }
    true
}

fn enable_dred() {
    let mut settings: Option<ID3D12DeviceRemovedExtendedDataSettings> = None;
    if unsafe { D3D12GetDebugInterface(&mut settings) }.is_ok() {
        let settings = settings.unwrap();
        unsafe {
            settings.SetPageFaultEnablement(D3D12_DRED_ENABLEMENT_FORCED_ON);
            settings.SetAutoBreadcrumbsEnablement(D3D12_DRED_ENABLEMENT_FORCED_ON);
        }
    }
}

/// Hardware device with DRED enabled, `None` when only WARP is available. Retries for a few
/// seconds: right after a device removal the adapter can refuse new devices while it resets.
fn hardware_device() -> Option<ID3D12Device> {
    enable_dred();
    let mut last_err = None;
    for attempt in 0..20 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        let factory: IDXGIFactory6 = unsafe { CreateDXGIFactory2(Default::default()) }.ok()?;
        for idx in 0.. {
            let adapter: IDXGIAdapter1 = match unsafe { factory.EnumAdapters1(idx) } {
                Ok(a) => a,
                Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(e) => panic!("EnumAdapters1: {e}"),
            };
            let desc = unsafe { adapter.GetDesc1() }.ok()?;
            if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
                continue;
            }
            let mut device: Option<ID3D12Device> = None;
            match unsafe { D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_11_0, &mut device) } {
                Ok(()) => {
                    eprintln!(
                        "adapter: {}",
                        String::from_utf16_lossy(&desc.Description).trim_end_matches('\0')
                    );
                    return device;
                }
                Err(e) => last_err = Some(e),
            }
        }
        if last_err.is_none() {
            break; // No hardware adapter at all, no point retrying.
        }
    }
    match last_err {
        Some(e) => eprintln!("skipped: hardware adapter refused device creation: {e}"),
        None => eprintln!("skipped: no hardware adapter, WARP does not page-fault"),
    }
    None
}

const CHILD_ENV: &str = "GPU_ALLOCATOR_STOMP_FAULT_CHILD";

/// Runs `body` in a child process. After the second device removal in one process the
/// adapter keeps refusing new devices with `DXGI_ERROR_DEVICE_HUNG`, so every test gets a
/// process of its own. In the child, `setup()` returns the device.
fn isolated(test_name: &str, body: fn()) {
    if std::env::var_os(CHILD_ENV).is_some() {
        body();
        return;
    }
    if !enabled() {
        return;
    }
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            test_name,
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENV, "1")
        .status()
        .expect("spawn child test process");
    assert!(
        status.success(),
        "child process for `{test_name}` failed: {status}"
    );
}

/// `Some(device)` only inside an `isolated` child with hardware present.
fn setup() -> Option<ID3D12Device> {
    hardware_device()
}

/// An `#[ignore]`d test whose body runs in its own process, see [`isolated`].
macro_rules! fault_test {
    ($name:ident, $body:block) => {
        #[test]
        #[ignore]
        fn $name() {
            isolated(stringify!($name), || $body);
        }
    };
}

fn create_queue(device: &ID3D12Device, ty: D3D12_COMMAND_LIST_TYPE) -> ID3D12CommandQueue {
    unsafe {
        device.CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
            Type: ty,
            ..Default::default()
        })
    }
    .expect("CreateCommandQueue")
}

/// Default stomp settings on a dedicated COPY queue; adjust fields on the result.
fn settings(device: &ID3D12Device) -> StompSettings {
    StompSettings::new(create_queue(device, D3D12_COMMAND_LIST_TYPE_COPY))
}

fn stomp_allocator(device: &ID3D12Device, settings: StompSettings) -> Allocator {
    Allocator::new(&AllocatorCreateDesc {
        device: ID3D12DeviceVersion::Device(device.clone()),
        debug_settings: Default::default(),
        allocation_sizes: Default::default(),
        stomp: Some(settings),
    })
    .expect("Allocator::new")
}

fn buffer_desc(width: u64, flags: D3D12_RESOURCE_FLAGS) -> D3D12_RESOURCE_DESC {
    D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
        Alignment: 0,
        Width: width,
        Height: 1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: DXGI_FORMAT_UNKNOWN,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        Flags: flags,
    }
}

fn tex2d_desc(width: u64, height: u32) -> D3D12_RESOURCE_DESC {
    D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
        Alignment: 0,
        Width: width,
        Height: height,
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: DXGI_FORMAT_R8G8B8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
        Flags: D3D12_RESOURCE_FLAG_NONE,
    }
}

fn create(
    allocator: &mut Allocator,
    name: &str,
    desc: &D3D12_RESOURCE_DESC,
    category: ResourceCategory,
) -> Resource {
    allocator
        .create_resource(&ResourceCreateDesc {
            name,
            memory_location: MemoryLocation::GpuOnly,
            resource_category: category,
            resource_desc: desc,
            castable_formats: &[],
            clear_value: None,
            initial_state_or_layout: ResourceStateOrBarrierLayout::ResourceState(
                D3D12_RESOURCE_STATE_COMMON,
            ),
            resource_type: &ResourceType::Placed,
            stomp: None,
        })
        .expect("create_resource")
}

fn compute_pso(
    device: &ID3D12Device,
    blob: &[u8],
) -> Result<(ID3D12RootSignature, ID3D12PipelineState)> {
    let root_signature: ID3D12RootSignature = unsafe { device.CreateRootSignature(0, blob) }?;
    let pso = unsafe {
        device.CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
            pRootSignature: std::mem::ManuallyDrop::new(Some(root_signature.clone())),
            CS: D3D12_SHADER_BYTECODE {
                pShaderBytecode: blob.as_ptr().cast(),
                BytecodeLength: blob.len(),
            },
            ..Default::default()
        })
    }?;
    Ok((root_signature, pso))
}

/// Direct queue plus fence, with root-descriptor read and write kernels.
struct Gpu {
    device: ID3D12Device,
    queue: ID3D12CommandQueue,
    fence: ID3D12Fence,
    value: u64,
    read: (ID3D12RootSignature, ID3D12PipelineState),
    write: (ID3D12RootSignature, ID3D12PipelineState),
    texture_load: (ID3D12RootSignature, ID3D12PipelineState),
    srv_heap: ID3D12DescriptorHeap,
    scratch: ID3D12Resource,
}

/// Outcome of one GPU submission.
#[derive(Debug)]
enum Outcome {
    Completed,
    /// `Close()` or `ExecuteCommandLists` refused the work; the device is still alive.
    Rejected(windows::core::Error),
    /// `GetDeviceRemovedReason` failed after the submission.
    DeviceRemoved {
        reason: windows::core::Error,
        page_fault_va: u64,
        dred_names: Vec<String>,
    },
}

impl Gpu {
    fn new(device: &ID3D12Device) -> Self {
        let queue = create_queue(device, D3D12_COMMAND_LIST_TYPE_DIRECT);
        let fence = unsafe { device.CreateFence(0, D3D12_FENCE_FLAG_NONE) }.expect("CreateFence");
        let read = compute_pso(device, ROOT_SRV_READ_CS).expect("read pso");
        let write = compute_pso(device, ROOT_UAV_WRITE_CS).expect("write pso");
        let texture_load =
            compute_pso(device, TABLE_SRV_TEXTURE_LOAD_CS).expect("texture load pso");
        let srv_heap = unsafe {
            device.CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
                NumDescriptors: 1,
                Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                NodeMask: 0,
            })
        }
        .expect("CreateDescriptorHeap");
        let mut scratch: Option<ID3D12Resource> = None;
        unsafe {
            device.CreateCommittedResource(
                &D3D12_HEAP_PROPERTIES {
                    Type: D3D12_HEAP_TYPE_DEFAULT,
                    ..Default::default()
                },
                D3D12_HEAP_FLAG_NONE,
                &buffer_desc(256, D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS),
                D3D12_RESOURCE_STATE_COMMON,
                None,
                &mut scratch,
            )
        }
        .expect("scratch");
        Self {
            device: device.clone(),
            queue,
            fence,
            value: 0,
            read,
            write,
            texture_load,
            srv_heap,
            scratch: scratch.unwrap(),
        }
    }

    /// Records into a fresh list, executes, waits, then classifies what happened.
    fn run(&mut self, record: impl FnOnce(&ID3D12GraphicsCommandList)) -> Outcome {
        let submitted: Result<()> = (|| unsafe {
            let allocator: ID3D12CommandAllocator = self
                .device
                .CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)?;
            let list: ID3D12GraphicsCommandList = self.device.CreateCommandList(
                0,
                D3D12_COMMAND_LIST_TYPE_DIRECT,
                &allocator,
                None,
            )?;
            record(&list);
            list.Close()?;
            self.queue.ExecuteCommandLists(&[Some(list.cast()?)]);
            self.value += 1;
            self.queue.Signal(&self.fence, self.value)?;
            // A null event blocks until the fence reaches the value (or the device dies).
            self.fence
                .SetEventOnCompletion(self.value, HANDLE::default())?;
            Ok(())
        })();
        match unsafe { self.device.GetDeviceRemovedReason() } {
            Ok(()) => match submitted {
                Ok(()) => Outcome::Completed,
                Err(e) => Outcome::Rejected(e),
            },
            Err(reason) => {
                std::thread::sleep(std::time::Duration::from_millis(500));
                let (page_fault_va, dred_names) = dred_page_fault(&self.device);
                Outcome::DeviceRemoved {
                    reason,
                    page_fault_va,
                    dred_names,
                }
            }
        }
    }

    fn write_at(&mut self, va: u64) -> Outcome {
        let (rs, pso) = self.write.clone();
        self.run(|list| unsafe {
            list.SetComputeRootSignature(&rs);
            list.SetPipelineState(&pso);
            list.SetComputeRootUnorderedAccessView(0, va);
            list.Dispatch(1, 1, 1);
        })
    }

    fn read_at(&mut self, va: u64) -> Outcome {
        let (rs, pso) = self.read.clone();
        let scratch_va = unsafe { self.scratch.GetGPUVirtualAddress() };
        self.run(|list| unsafe {
            list.SetComputeRootSignature(&rs);
            list.SetPipelineState(&pso);
            list.SetComputeRootShaderResourceView(0, va);
            list.SetComputeRootUnorderedAccessView(1, scratch_va);
            list.Dispatch(1, 1, 1);
        })
    }

    /// Writes an SRV for `texture` into the descriptor heap. Done before the texture is freed
    /// so the descriptor goes stale, exactly like a real use-after-free.
    fn make_texture_srv(&self, texture: &ID3D12Resource) {
        unsafe {
            self.device.CreateShaderResourceView(
                texture,
                None,
                self.srv_heap.GetCPUDescriptorHandleForHeapStart(),
            )
        }
    }

    /// Loads texel (0,0) through whatever SRV is in the descriptor heap.
    fn load_texture(&mut self) -> Outcome {
        let (rs, pso) = self.texture_load.clone();
        let heap = self.srv_heap.clone();
        let table = unsafe { heap.GetGPUDescriptorHandleForHeapStart() };
        let scratch_va = unsafe { self.scratch.GetGPUVirtualAddress() };
        self.run(|list| unsafe {
            list.SetDescriptorHeaps(&[Some(heap.clone())]);
            list.SetComputeRootSignature(&rs);
            list.SetPipelineState(&pso);
            list.SetComputeRootDescriptorTable(0, table);
            list.SetComputeRootUnorderedAccessView(1, scratch_va);
            list.Dispatch(1, 1, 1);
        })
    }
}

fn dred_page_fault(device: &ID3D12Device) -> (u64, Vec<String>) {
    let Ok(dred) = device.cast::<ID3D12DeviceRemovedExtendedData>() else {
        return (0, vec![]);
    };
    let Ok(out) = (unsafe { dred.GetPageFaultAllocationOutput() }) else {
        return (0, vec![]);
    };
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
    (out.PageFaultVA, names)
}

fn assert_removed(outcome: Outcome, expected_name: &str) {
    match outcome {
        Outcome::DeviceRemoved {
            reason,
            page_fault_va,
            dred_names,
        } => {
            eprintln!("device removed: {reason}");
            if dred_names.iter().any(|n| n == expected_name) {
                eprintln!("DRED named `{expected_name}` at VA {page_fault_va:#x}");
            } else {
                // Informational: NVIDIA reports VA 0 and no names, see file header.
                eprintln!(
                    "DRED did not attribute the fault (VA {page_fault_va:#x}, names {dred_names:?})"
                );
            }
        }
        other => panic!("expected device removal, got {other:?}"),
    }
}

fn assert_completed(outcome: Outcome) {
    match outcome {
        Outcome::Completed => {}
        other => panic!("expected completion, got {other:?}"),
    }
}

fault_test!(tail_overrun_faults, {
    let Some(device) = setup() else { return };
    let mut allocator = stomp_allocator(&device, settings(&device));
    let desc = buffer_desc(4000, D3D12_RESOURCE_FLAG_NONE);
    let resource = create(
        &mut allocator,
        "stomp-tail",
        &desc,
        ResourceCategory::Buffer,
    );
    assert!(resource.is_stomp_guarded());
    let layout = resource.stomp_layout().unwrap();
    let guard_va = layout.tail_guard_va().unwrap();
    assert_eq!(
        guard_va,
        layout.resource_va + TILE,
        "one payload tile, then the guard"
    );

    let mut gpu = Gpu::new(&device);
    assert_removed(gpu.write_at(guard_va), "stomp-tail");
    allocator.free_resource(resource).unwrap();
});

fault_test!(in_bounds_write_does_not_fault, {
    let Some(device) = setup() else { return };
    let mut allocator = stomp_allocator(&device, settings(&device));
    let desc = buffer_desc(4000, D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS);
    let resource = create(&mut allocator, "in-bounds", &desc, ResourceCategory::Buffer);
    assert!(resource.is_stomp_guarded());
    let va = unsafe { resource.resource().GetGPUVirtualAddress() } + resource.offset();
    assert_eq!(
        resource.offset(),
        0,
        "default settings keep the payload at offset 0"
    );

    let mut gpu = Gpu::new(&device);
    assert_completed(gpu.write_at(va));
    // Last 4 bytes of the payload.
    assert_completed(gpu.write_at(va + 4000 - 4));
    assert_completed(gpu.read_at(va));
    allocator.free_resource(resource).unwrap();
});

fault_test!(head_underrun_faults, {
    let Some(device) = setup() else { return };
    let mut settings = settings(&device);
    settings.head_guard = true;
    settings.tail_guard = false;
    let mut allocator = stomp_allocator(&device, settings);
    let desc = buffer_desc(4000, D3D12_RESOURCE_FLAG_NONE);
    let resource = create(
        &mut allocator,
        "stomp-head",
        &desc,
        ResourceCategory::Buffer,
    );
    assert!(resource.is_stomp_guarded());
    let layout = resource.stomp_layout().unwrap();
    assert_eq!(
        layout.offset, TILE,
        "payload starts after the head guard tile"
    );
    assert_eq!(resource.offset(), TILE);
    assert_eq!(layout.head_guard_va(), Some(layout.resource_va));
    assert_eq!(layout.tail_guard_va(), None);
    let payload_va = layout.resource_va + layout.offset;

    let mut gpu = Gpu::new(&device);
    // First bytes of the payload are fine.
    assert_completed(gpu.read_at(payload_va));
    // 64 bytes before the payload, inside the head guard.
    assert_removed(gpu.read_at(payload_va - 64), "stomp-head");
    allocator.free_resource(resource).unwrap();
});

fault_test!(slack_boundary_default_is_tile_precise, {
    let Some(device) = setup() else { return };
    let mut allocator = stomp_allocator(&device, settings(&device));
    let desc = buffer_desc(4000, D3D12_RESOURCE_FLAG_NONE);
    let resource = create(&mut allocator, "slack", &desc, ResourceCategory::Buffer);
    let layout = resource.stomp_layout().unwrap();
    assert_eq!(layout.offset, 0);
    let guard_va = layout.tail_guard_va().unwrap();

    let mut gpu = Gpu::new(&device);
    // Past the requested 4000 bytes but still inside the payload tile: slack, no fault.
    assert_completed(gpu.write_at(layout.resource_va + 4000));
    assert_completed(gpu.write_at(guard_va - 4));
    assert_removed(gpu.write_at(guard_va), "slack");
    allocator.free_resource(resource).unwrap();
});

fault_test!(slack_boundary_with_payload_alignment_is_byte_precise, {
    let Some(device) = setup() else { return };
    let mut settings = settings(&device);
    settings.payload_alignment = Some(8);
    let mut allocator = stomp_allocator(&device, settings);
    let desc = buffer_desc(4000, D3D12_RESOURCE_FLAG_NONE);
    let resource = create(&mut allocator, "packed", &desc, ResourceCategory::Buffer);
    let layout = resource.stomp_layout().unwrap();
    assert_eq!(layout.offset, TILE - 4000);
    assert_eq!(resource.offset(), TILE - 4000);
    let payload_va = layout.resource_va + layout.offset;
    let guard_va = layout.tail_guard_va().unwrap();
    assert_eq!(
        payload_va + 4000,
        guard_va,
        "payload ends exactly at the guard"
    );

    let mut gpu = Gpu::new(&device);
    assert_completed(gpu.write_at(payload_va));
    // Last 4 bytes of the payload.
    assert_completed(gpu.write_at(payload_va + 4000 - 4));
    // First byte past the payload is the guard.
    assert_removed(gpu.write_at(guard_va), "packed");
    allocator.free_resource(resource).unwrap();
});

fault_test!(buffer_use_after_free_faults, {
    let Some(device) = setup() else { return };
    let mut allocator = stomp_allocator(&device, settings(&device));
    let desc = buffer_desc(4000, D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS);
    let resource = create(&mut allocator, "stale", &desc, ResourceCategory::Buffer);
    let va = unsafe { resource.resource().GetGPUVirtualAddress() } + resource.offset();

    let mut gpu = Gpu::new(&device);
    assert_completed(gpu.write_at(va));

    allocator.free_resource(resource).unwrap();
    let stats = allocator.stomp_statistics().unwrap();
    assert_eq!(stats.quarantined, 1);
    // Same VA, resource kept alive by the quarantine, all tiles now never-resident.
    assert_removed(gpu.write_at(va), "stale");
});

fault_test!(texture_use_after_free_faults, {
    let Some(device) = setup() else { return };
    let mut allocator = stomp_allocator(&device, settings(&device));
    let desc = tex2d_desc(64, 64);
    let resource = create(
        &mut allocator,
        "stale-tex",
        &desc,
        ResourceCategory::OtherTexture,
    );
    assert!(
        resource.stomp_layout().is_some(),
        "texture took the stomp path"
    );
    assert!(!resource.is_stomp_guarded(), "textures have no guard tile");

    let mut gpu = Gpu::new(&device);
    gpu.make_texture_srv(resource.resource());
    assert_completed(gpu.load_texture());

    allocator.free_resource(resource).unwrap();
    assert_eq!(allocator.stomp_statistics().unwrap().quarantined, 1);
    // Stale descriptor: the resource is alive but every tile maps to the never-resident heap.
    assert_removed(gpu.load_texture(), "stale-tex");
});

fault_test!(freed_without_quarantine_is_released, {
    let Some(device) = setup() else { return };
    let mut settings = settings(&device);
    settings.quarantine = false;
    let mut allocator = stomp_allocator(&device, settings);
    let desc = buffer_desc(4000, D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS);
    let resource = create(&mut allocator, "released", &desc, ResourceCategory::Buffer);
    let va = unsafe { resource.resource().GetGPUVirtualAddress() };

    let mut gpu = Gpu::new(&device);
    assert_completed(gpu.write_at(va));
    allocator.free_resource(resource).unwrap();
    assert_eq!(allocator.stomp_statistics().unwrap().quarantined, 0);
    // Nothing to assert about faults: the VA is unmapped or reused, driver dependent.
    eprintln!("stats after release: {:?}", allocator.stomp_statistics());
});

fault_test!(oversized_copy_is_rejected_at_close, {
    // Documents why the other tests use root descriptors: the runtime bounds-checks copies.
    let Some(device) = setup() else { return };
    let mut allocator = stomp_allocator(&device, settings(&device));
    let dst = create(
        &mut allocator,
        "copy-dst",
        &buffer_desc(4000, D3D12_RESOURCE_FLAG_NONE),
        ResourceCategory::Buffer,
    );
    let src = create(
        &mut allocator,
        "copy-src",
        &buffer_desc(TILE * 3, D3D12_RESOURCE_FLAG_NONE),
        ResourceCategory::Buffer,
    );
    // The reserved buffer is 2 tiles wide (payload plus tail guard); copy 3 tiles into it.
    let dst_width = unsafe { dst.resource().GetDesc() }.Width;
    assert_eq!(dst_width, 2 * TILE);

    let mut gpu = Gpu::new(&device);
    let outcome = gpu.run(|list| unsafe {
        list.CopyBufferRegion(dst.resource(), 0, src.resource(), 0, dst_width + TILE)
    });
    match outcome {
        Outcome::Rejected(e) => eprintln!("runtime rejected the oversized copy: {e}"),
        Outcome::DeviceRemoved { .. } => {
            eprintln!("driver executed the oversized copy and faulted")
        }
        Outcome::Completed => panic!("oversized copy neither rejected nor faulted"),
    }
    allocator.free_resource(src).unwrap();
    allocator.free_resource(dst).unwrap();
});
