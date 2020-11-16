use ash::version::{DeviceV1_0, EntryV1_0, InstanceV1_0};
use ash::vk;

use std::default::Default;
use std::ffi::CString;

use gpu_allocator::{
    AllocationCreateDesc, MemoryLocation, VulkanAllocator, VulkanAllocatorCreateDesc,
};

fn main() {
    let entry = ash::Entry::new().unwrap();

    let event_loop = winit::event_loop::EventLoop::new();

    let window_width = 1024;
    let window_height = 768;
    let window = winit::window::WindowBuilder::new()
        .with_title("gpu-allocator vulkan visualization")
        .with_inner_size(winit::dpi::PhysicalSize::new(
            window_width as f64,
            window_height as f64,
        ))
        .with_resizable(false)
        .build(&event_loop)
        .unwrap();

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

        let surface_extensions = ash_window::enumerate_required_extensions(&window).unwrap();
        let extensions_names_raw = surface_extensions
            .iter()
            .map(|ext| ext.as_ptr())
            .collect::<Vec<_>>();

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

    let surface = unsafe { ash_window::create_surface(&entry, &instance, &window, None) }.unwrap();
    let surface_loader = ash::extensions::khr::Surface::new(&entry, &instance);

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
                        let supports_surface = unsafe {
                            surface_loader.get_physical_device_surface_support(
                                *pdevice,
                                index as u32,
                                surface,
                            )
                        }
                        .unwrap();
                        if supports_graphics && supports_surface {
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

    let present_queue = unsafe { device.get_device_queue(queue_family_index as u32, 0) };

    //create queue and swapchain and surface?
    let surface_format =
        unsafe { surface_loader.get_physical_device_surface_formats(pdevice, surface) }.unwrap()[0];
    let surface_capabilities =
        unsafe { surface_loader.get_physical_device_surface_capabilities(pdevice, surface) }
            .unwrap();
    let mut desired_image_count = surface_capabilities.min_image_count + 1;
    if surface_capabilities.max_image_count > 0
        && desired_image_count > surface_capabilities.max_image_count
    {
        desired_image_count = surface_capabilities.max_image_count;
    }
    let surface_resolution = match surface_capabilities.current_extent.width {
        std::u32::MAX => vk::Extent2D {
            width: window_width,
            height: window_height,
        },
        _ => surface_capabilities.current_extent,
    };
    let pre_transform = if surface_capabilities
        .supported_transforms
        .contains(vk::SurfaceTransformFlagsKHR::IDENTITY)
    {
        vk::SurfaceTransformFlagsKHR::IDENTITY
    } else {
        surface_capabilities.current_transform
    };
    let present_modes =
        unsafe { surface_loader.get_physical_device_surface_present_modes(pdevice, surface) }
            .unwrap();
    let present_mode = present_modes
        .iter()
        .cloned()
        .find(|&mode| mode == vk::PresentModeKHR::MAILBOX)
        .unwrap_or(vk::PresentModeKHR::FIFO);
    let swapchain_loader = ash::extensions::khr::Swapchain::new(&instance, &device);

    let swapchain_create_info = vk::SwapchainCreateInfoKHR::builder()
        .surface(surface)
        .min_image_count(desired_image_count)
        .image_color_space(surface_format.color_space)
        .image_format(surface_format.format)
        .image_extent(surface_resolution)
        .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
        .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
        .pre_transform(pre_transform)
        .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
        .present_mode(present_mode)
        .clipped(true)
        .image_array_layers(1);
    let swapchain =
        unsafe { swapchain_loader.create_swapchain(&swapchain_create_info, None) }.unwrap();

    let pool_create_info = vk::CommandPoolCreateInfo::builder()
        .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
        .queue_family_index(queue_family_index as u32);
    let pool = unsafe { device.create_command_pool(&pool_create_info, None) }.unwrap();

    let command_buffer_allocate_info = vk::CommandBufferAllocateInfo::builder()
        .command_buffer_count(2)
        .command_pool(pool)
        .level(vk::CommandBufferLevel::PRIMARY);

    let command_buffers =
        unsafe { device.allocate_command_buffers(&command_buffer_allocate_info) }.unwrap();
    let setup_command_buffer = command_buffers[0];
    let draw_command_buffer = command_buffers[1];

    let present_images = unsafe { swapchain_loader.get_swapchain_images(swapchain) }.unwrap();
    let present_image_views = present_images
        .iter()
        .map(|&image| {
            let create_view_info = vk::ImageViewCreateInfo::builder()
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(surface_format.format)
                .components(vk::ComponentMapping {
                    r: vk::ComponentSwizzle::R,
                    g: vk::ComponentSwizzle::G,
                    b: vk::ComponentSwizzle::B,
                    a: vk::ComponentSwizzle::A,
                })
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .image(image);
            unsafe { device.create_image_view(&create_view_info, None) }.unwrap()
        })
        .collect::<Vec<_>>();

    let device_memory_properties =
        unsafe { instance.get_physical_device_memory_properties(pdevice) };
    let depth_image_create_info = vk::ImageCreateInfo::builder()
        .image_type(vk::ImageType::TYPE_2D)
        .format(vk::Format::D16_UNORM)
        .extent(vk::Extent3D {
            width: surface_resolution.width,
            height: surface_resolution.height,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::OPTIMAL)
        .usage(vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT)
        .sharing_mode(vk::SharingMode::EXCLUSIVE);
    let depth_image = unsafe { device.create_image(&depth_image_create_info, None) }.unwrap();
    let depth_image_memory_requirements =
        unsafe { device.get_image_memory_requirements(depth_image) };

    // Setting up the allocator
    let mut allocator = VulkanAllocator::new(&VulkanAllocatorCreateDesc {
        instance,
        device: device.clone(),
        physical_device: pdevice,
        debug_settings: Default::default(),
    });

    let depth_image_allocation = allocator
        .allocate(&AllocationCreateDesc {
            name: "swapchain image",
            requirements: depth_image_memory_requirements,
            location: MemoryLocation::GpuOnly,
            linear: false,
        })
        .unwrap();

    unsafe {
        device.bind_image_memory(
            depth_image,
            depth_image_allocation.memory(),
            depth_image_allocation.offset(),
        )
    }
    .unwrap();

    let fence_create_info = vk::FenceCreateInfo::builder().flags(vk::FenceCreateFlags::SIGNALED);
    let draw_commands_reuse_fence = unsafe { device.create_fence(&fence_create_info, None) }.unwrap();
    let setup_commands_reuse_fence = unsafe { device.create_fence(&fence_create_info, None) }.unwrap();

    // Test allocating GPU Only memory
    {
        let test_buffer_info = vk::BufferCreateInfo::builder()
            .size(512)
            .usage(vk::BufferUsageFlags::STORAGE_BUFFER)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        let test_buffer = unsafe { device.create_buffer(&test_buffer_info, None) }.unwrap();
        let requirements = unsafe { device.get_buffer_memory_requirements(test_buffer) };
        let location = MemoryLocation::GpuOnly;

        let allocation = allocator
            .allocate(&AllocationCreateDesc {
                requirements,
                location,
                linear: true,
                name: "test allocation",
            })
            .unwrap();

        unsafe {
            device
                .bind_buffer_memory(test_buffer, allocation.memory(), allocation.offset())
                .unwrap()
        };

        allocator.free(allocation).unwrap();

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
        let location = MemoryLocation::CpuToGpu;

        let allocation = allocator
            .allocate(&AllocationCreateDesc {
                requirements,
                location,
                linear: true,
                name: "test allocation",
            })
            .unwrap();

        unsafe {
            device
                .bind_buffer_memory(test_buffer, allocation.memory(), allocation.offset())
                .unwrap()
        };

        allocator.free(allocation).unwrap();

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
        let location = MemoryLocation::GpuToCpu;

        let allocation = allocator
            .allocate(&AllocationCreateDesc {
                requirements,
                location,
                linear: true,
                name: "test allocation",
            })
            .unwrap();

        unsafe {
            device
                .bind_buffer_memory(test_buffer, allocation.memory(), allocation.offset())
                .unwrap()
        };

        allocator.free(allocation).unwrap();

        unsafe { device.destroy_buffer(test_buffer, None) };

        println!("Allocation and deallocation of GpuToCpu memory was successful.");
    }
}
