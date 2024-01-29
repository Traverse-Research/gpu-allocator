use std::sync::Arc;

use gpu_allocator::metal::{AllocationCreateDesc, Allocator, AllocatorCreateDesc};
use log::info;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("trace")).init();

    let device = Arc::new(metal::Device::system_default().unwrap());

    // Setting up the allocator
    let mut allocator = Allocator::new(&AllocatorCreateDesc {
        device: device.clone(),
        debug_settings: Default::default(),
        allocation_sizes: Default::default(),
    })
    .unwrap();

    // Test allocating Gpu Only memory
    {
        let allocation_desc = AllocationCreateDesc::buffer(
            &device,
            "Test allocation (Gpu Only)",
            512,
            gpu_allocator::MemoryLocation::GpuOnly,
        );
        let allocation = allocator.allocate(&allocation_desc).unwrap();
        let _buffer = allocation.make_buffer(512).unwrap();
        allocator.free(allocation).unwrap();
        info!("Allocation and deallocation of GpuOnly memory was successful.");
    }

    // Test allocating Cpu to Gpu memory
    {
        let allocation_desc = AllocationCreateDesc::buffer(
            &device,
            "Test allocation (Cpu to Gpu)",
            512,
            gpu_allocator::MemoryLocation::CpuToGpu,
        );
        let allocation = allocator.allocate(&allocation_desc).unwrap();
        let _buffer = allocation.make_buffer(512).unwrap();
        allocator.free(allocation).unwrap();
        info!("Allocation and deallocation of CpuToGpu memory was successful.");
    }

    // Test allocating Gpu to Cpu memory
    {
        let allocation_desc = AllocationCreateDesc::buffer(
            &device,
            "Test allocation (Gpu to Cpu)",
            512,
            gpu_allocator::MemoryLocation::GpuToCpu,
        );
        let allocation = allocator.allocate(&allocation_desc).unwrap();
        let _buffer = allocation.make_buffer(512).unwrap();
        allocator.free(allocation).unwrap();
        info!("Allocation and deallocation of GpuToCpu memory was successful.");
    }

    // Test allocating texture
    {
        let texture_desc = metal::TextureDescriptor::new();
        texture_desc.set_pixel_format(metal::MTLPixelFormat::RGBA8Unorm);
        texture_desc.set_width(64);
        texture_desc.set_height(64);
        texture_desc.set_storage_mode(metal::MTLStorageMode::Private);
        let allocation_desc = AllocationCreateDesc::from_texture_descriptor(
            &device,
            "Test allocation (Texture)",
            &texture_desc,
        );
        let allocation = allocator.allocate(&allocation_desc).unwrap();
        let _texture = allocation.make_texture(&texture_desc).unwrap();
        allocator.free(allocation).unwrap();
        info!("Allocation and deallocation of Texture was successful.");
    }
}
