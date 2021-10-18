use gpu_allocator::d3d12::{
    AllocationCreateDesc, Allocator, AllocatorCreateDesc, Dx12DevicePtr, ResourceCategory,
};
use gpu_allocator::MemoryLocation;
use log::*;
use windows::{Abi, Interface, Result};
use windows_bindings::Windows::Win32::{
    Foundation::E_NOINTERFACE,
    Graphics::{
        Direct3D11::{D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_12_0},
        Direct3D12::{
            D3D12CreateDevice, ID3D12Device, ID3D12Heap, ID3D12Resource, D3D12_RESOURCE_DESC,
            D3D12_RESOURCE_DIMENSION_BUFFER, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_COMMON,
            D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        },
        Dxgi::{
            CreateDXGIFactory2, IDXGIAdapter4, IDXGIFactory6, DXGI_ADAPTER_FLAG3_SOFTWARE,
            DXGI_ERROR_NOT_FOUND, DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC,
        },
    },
};

fn create_d3d12_device(dxgi_factory: &IDXGIFactory6) -> Option<ID3D12Device> {
    for idx in 0.. {
        // TODO: Might as well return Result<> from this function
        let adapter1 = match unsafe { dxgi_factory.EnumAdapters1(idx) } {
            Ok(a) => a,
            Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(e) => panic!("{:?}", e),
        };
        let adapter4: IDXGIAdapter4 = adapter1.cast().unwrap();

        let desc = unsafe { adapter4.GetDesc3() }.unwrap();
        // Skip software adapters
        // Vote for https://github.com/microsoft/windows-rs/issues/793!
        if (desc.Flags & DXGI_ADAPTER_FLAG3_SOFTWARE) == DXGI_ADAPTER_FLAG3_SOFTWARE {
            continue;
        }

        let feature_levels = [
            (D3D_FEATURE_LEVEL_11_0, "D3D_FEATURE_LEVEL_11_0"),
            (D3D_FEATURE_LEVEL_11_1, "D3D_FEATURE_LEVEL_11_1"),
            (D3D_FEATURE_LEVEL_12_0, "D3D_FEATURE_LEVEL_12_0"),
        ];

        let device =
            feature_levels
                .iter()
                .rev()
                .find_map(|&(feature_level, feature_level_name)| {
                    let mut device = None;
                    match unsafe { D3D12CreateDevice(&adapter4, feature_level, &mut device) } {
                        Ok(()) => {
                            info!("Using D3D12 feature level: {}", feature_level_name);
                            Some(device.unwrap())
                        }
                        Err(e) if e.code() == E_NOINTERFACE => {
                            error!("ID3D12Device interface not supported");
                            None
                        }
                        Err(e) => {
                            info!(
                                "D3D12 feature level {} not supported: {}",
                                feature_level_name, e
                            );
                            None
                        }
                    }
                });
        if device.is_some() {
            return device;
        }
    }

    None
}

fn main() -> Result<()> {
    let dxgi_factory = unsafe { CreateDXGIFactory2(0) }?;

    let device = create_d3d12_device(&dxgi_factory).expect("Failed to create D3D12 device.");
    let device_ptr =
        unsafe { windows::runtime::IntoParam::<ID3D12Device>::into_param(device.clone()).abi() };

    // Setting up the allocator
    let mut allocator = Allocator::new(&AllocatorCreateDesc {
        device: Dx12DevicePtr(device_ptr),
        debug_settings: Default::default(),
    })
    .unwrap();

    // Test allocating GPU Only memory
    {
        let test_buffer_desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Alignment: 0, // alias for D3D12_DEFAULT_RESOURCE_PLACEMENT_ALIGNMENT
            Width: 512,
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

        let allocation_desc = AllocationCreateDesc::from_d3d12_resource_desc(
            allocator.device(),
            // Raw structs are binary-compatible
            unsafe { std::mem::transmute(&test_buffer_desc) },
            "test allocation",
            MemoryLocation::GpuOnly,
        );
        let allocation = allocator.allocate(&allocation_desc).unwrap();

        let mut resource: Option<ID3D12Resource> = None;
        unsafe {
            device.CreatePlacedResource(
                ID3D12Heap::from_abi(allocation.heap().0)?,
                allocation.offset(),
                &test_buffer_desc,
                D3D12_RESOURCE_STATE_COMMON,
                std::ptr::null(),
                &mut resource,
            )
        }?;

        drop(resource);

        allocator.free(allocation).unwrap();
        info!("Allocation and deallocation of GpuOnly memory was successful.");
    }

    // Test allocating CPU to GPU memory
    {
        let test_buffer_desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Alignment: 0, // alias for D3D12_DEFAULT_RESOURCE_PLACEMENT_ALIGNMENT
            Width: 512,
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

        let alloc_info = unsafe { device.GetResourceAllocationInfo(0, 1, &test_buffer_desc) };

        let allocation = allocator
            .allocate(&AllocationCreateDesc {
                name: "test allocation",
                location: MemoryLocation::CpuToGpu,
                size: alloc_info.SizeInBytes,
                alignment: alloc_info.Alignment,
                resource_category: ResourceCategory::Buffer,
            })
            .unwrap();

        let mut resource: Option<ID3D12Resource> = None;
        unsafe {
            device.CreatePlacedResource(
                ID3D12Heap::from_abi(allocation.heap().0)?,
                allocation.offset(),
                &test_buffer_desc,
                D3D12_RESOURCE_STATE_COMMON,
                std::ptr::null(),
                &mut resource,
            )
        }?;

        drop(resource);

        allocator.free(allocation).unwrap();
        info!("Allocation and deallocation of CpuToGpu memory was successful.");
    }

    // Test allocating GPU to CPU memory
    {
        let test_buffer_desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Alignment: 0, // alias for D3D12_DEFAULT_RESOURCE_PLACEMENT_ALIGNMENT
            Width: 512,
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

        let alloc_info = unsafe { device.GetResourceAllocationInfo(0, 1, &test_buffer_desc) };

        let allocation = allocator
            .allocate(&AllocationCreateDesc {
                name: "test allocation",
                location: MemoryLocation::GpuToCpu,
                size: alloc_info.SizeInBytes,
                alignment: alloc_info.Alignment,
                resource_category: ResourceCategory::Buffer,
            })
            .unwrap();

        let mut resource: Option<ID3D12Resource> = None;
        unsafe {
            device.CreatePlacedResource(
                ID3D12Heap::from_abi(allocation.heap().0)?,
                allocation.offset(),
                &test_buffer_desc,
                D3D12_RESOURCE_STATE_COMMON,
                std::ptr::null(),
                &mut resource,
            )
        }?;

        drop(resource);

        allocator.free(allocation).unwrap();
        info!("Allocation and deallocation of CpuToGpu memory was successful.");
    }

    drop(allocator); // Explicitly drop before destruction of device.

    Ok(())
}
