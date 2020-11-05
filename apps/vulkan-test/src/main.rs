use ash::version::{DeviceV1_0, EntryV1_0, InstanceV1_0};
use ash::vk;

use std::default::Default;
use std::ffi::CString;

fn main() {
    let entry = ash::Entry::new().unwrap();

    // Create vulkan instance
    let instance = {
        let app_name = CString::new("Vulkan gpu-allocator test").unwrap();

        let appinfo = vk::ApplicationInfo::builder()
            .application_name(&app_name)
            .application_version(0)
            .engine_name(&app_name)
            .engine_version(0)
            .api_version(vk::make_version(1, 0, 0));

        let layer_names = [CString::new("VK_LAYER_KHRONOS_validation").unwrap()];
        let layers_names_raw: Vec<*const i8> = layer_names
            .iter()
            .map(|raw_name| raw_name.as_ptr())
            .collect();

        let extensions_names_raw = vec![];

        let create_info = vk::InstanceCreateInfo::builder()
            .application_info(&appinfo)
            .enabled_layer_names(&layers_names_raw)
            .enabled_extension_names(&extensions_names_raw);

        unsafe {
            entry
                .create_instance(&create_info, None)
                .expect("Instance creation error")
        }
    };

    // Look for vulkan physical device
    let (pdevice, queue_family_index) = {
        let pdevices = unsafe {
            instance
                .enumerate_physical_devices()
                .expect("Physical device error")
        };
        pdevices
            .iter()
            .map(|pdevice| {
                unsafe { instance.get_physical_device_queue_family_properties(*pdevice) }
                    .iter()
                    .enumerate()
                    .filter_map(|(index, &info)| {
                        let supports_graphics = info.queue_flags.contains(vk::QueueFlags::GRAPHICS);
                        if supports_graphics {
                            Some((*pdevice, index))
                        } else {
                            None
                        }
                    })
                    .next()
            })
            .filter_map(|v| v)
            .next()
            .expect("Couldn't find suitable device.")
    };

    // Create vulkan device
    let device = {
        let device_extension_names_raw = vec![];
        let features = vk::PhysicalDeviceFeatures {
            shader_clip_distance: 1,
            ..Default::default()
        };
        let priorities = [1.0];

        let queue_info = [vk::DeviceQueueCreateInfo::builder()
            .queue_family_index(queue_family_index as u32)
            .queue_priorities(&priorities)
            .build()];

        let create_info = vk::DeviceCreateInfo::builder()
            .queue_create_infos(&queue_info)
            .enabled_extension_names(&device_extension_names_raw)
            .enabled_features(&features);

        unsafe { instance.create_device(pdevice, &create_info, None).unwrap() }
    };

    // Setting up the allocator
    let physical_device_properties2 = vk::PhysicalDeviceProperties2::default();
    let allocator =
        gpu_allocator::Allocator::new(&device, pdevice, &instance, physical_device_properties2)
            .unwrap();

    // Test allocating GPU Only memory
    {
        let test_buffer_info = vk::BufferCreateInfo::builder()
            .size(512)
            .usage(vk::BufferUsageFlags::STORAGE_BUFFER)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        let test_buffer = unsafe { device.create_buffer(&test_buffer_info, None) }.unwrap();
        let requirements = unsafe { device.get_buffer_memory_requirements(test_buffer) };
        let location = gpu_allocator::MemoryLocation::GpuOnly;

        let allocation = allocator
            .alloc(&gpu_allocator::AllocationCreateDesc {
                requirements,
                location,
                is_linear_resource: true,
                name: "test allocation",
            })
            .unwrap();

        allocator.free(&allocation).unwrap();

        unsafe { device.destroy_buffer(test_buffer, None) };

        println!("Allocation and deallocation of GpuOnly memory was successful.");
    }

    // Test allocating CPU to GPU memory
    {
        let test_buffer_info = vk::BufferCreateInfo::builder()
            .size(512)
            .usage(vk::BufferUsageFlags::STORAGE_BUFFER)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        let test_buffer = unsafe { device.create_buffer(&test_buffer_info, None) }.unwrap();
        let requirements = unsafe { device.get_buffer_memory_requirements(test_buffer) };
        let location = gpu_allocator::MemoryLocation::CpuToGpu;

        let allocation = allocator
            .alloc(&gpu_allocator::AllocationCreateDesc {
                requirements,
                location,
                is_linear_resource: true,
                name: "test allocation",
            })
            .unwrap();

        allocator.free(&allocation).unwrap();

        unsafe { device.destroy_buffer(test_buffer, None) };

        println!("Allocation and deallocation of CpuToGpu memory was successful.");
    }

    // Test allocating GPU to CPU memory
    {
        let test_buffer_info = vk::BufferCreateInfo::builder()
            .size(512)
            .usage(vk::BufferUsageFlags::STORAGE_BUFFER)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        let test_buffer = unsafe { device.create_buffer(&test_buffer_info, None) }.unwrap();
        let requirements = unsafe { device.get_buffer_memory_requirements(test_buffer) };
        let location = gpu_allocator::MemoryLocation::GpuToCpu;

        let allocation = allocator
            .alloc(&gpu_allocator::AllocationCreateDesc {
                requirements,
                location,
                is_linear_resource: true,
                name: "test allocation",
            })
            .unwrap();

        allocator.free(&allocation).unwrap();

        unsafe { device.destroy_buffer(test_buffer, None) };

        println!("Allocation and deallocation of GpuToCpu memory was successful.");
    }
}
