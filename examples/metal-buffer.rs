use gpu_allocator::metal::{AllocationCreateDesc, Allocator, AllocatorCreateDesc};
use log::info;
use metal::MTLDevice as _;
use objc2::rc::Id;
use objc2_foundation::NSArray;
use objc2_metal as metal;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("trace")).init();

    // Allow the innards of objc2-metal to link the static function below:
    // https://docs.rs/objc2-metal/0.2.2/objc2_metal/index.html
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {}

    let device =
        unsafe { Id::from_raw(metal::MTLCreateSystemDefaultDevice()) }.expect("No MTLDevice found");

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
        let _buffer = allocation.make_buffer().unwrap();
        allocator.free(&allocation).unwrap();
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
        let _buffer = allocation.make_buffer().unwrap();
        allocator.free(&allocation).unwrap();
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
        let _buffer = allocation.make_buffer().unwrap();
        allocator.free(&allocation).unwrap();
        info!("Allocation and deallocation of GpuToCpu memory was successful.");
    }

    // Test allocating texture
    {
        let texture_desc = unsafe { metal::MTLTextureDescriptor::new() };
        texture_desc.setPixelFormat(metal::MTLPixelFormat::RGBA8Unorm);
        unsafe { texture_desc.setWidth(64) };
        unsafe { texture_desc.setHeight(64) };
        texture_desc.setStorageMode(metal::MTLStorageMode::Private);
        let allocation_desc =
            AllocationCreateDesc::texture(&device, "Test allocation (Texture)", &texture_desc);
        let allocation = allocator.allocate(&allocation_desc).unwrap();
        let _texture = allocation.make_texture(&texture_desc).unwrap();
        allocator.free(&allocation).unwrap();
        info!("Allocation and deallocation of Texture was successful.");
    }

    // Test allocating acceleration structure
    {
        let empty_array = NSArray::from_slice(&[]);
        let acc_desc = metal::MTLPrimitiveAccelerationStructureDescriptor::descriptor();
        acc_desc.setGeometryDescriptors(Some(&empty_array));
        let sizes = device.accelerationStructureSizesWithDescriptor(&acc_desc);
        let allocation_desc = AllocationCreateDesc::acceleration_structure_with_size(
            &device,
            "Test allocation (Acceleration structure)",
            sizes.accelerationStructureSize as u64,
            gpu_allocator::MemoryLocation::GpuOnly,
        );
        let allocation = allocator.allocate(&allocation_desc).unwrap();
        let _acc_structure = allocation.make_acceleration_structure();
        allocator.free(&allocation).unwrap();
        info!("Allocation and deallocation of Acceleration structure was successful.");
    }
}
