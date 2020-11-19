use ash::version::{DeviceV1_0, EntryV1_0, InstanceV1_0};
use ash::vk;

use std::default::Default;
use std::ffi::CString;

mod shaders;

use gpu_allocator::{
    AllocationCreateDesc, MemoryLocation, VulkanAllocator, VulkanAllocatorCreateDesc,
};

pub struct ImGuiRenderer {
    //size: (u32, u32),
    //render_pass: Arc<dyn RenderPass>,
    //pipeline: Arc<dyn Pipeline>,
    //sampler: Arc<dyn Sampler>,
    //font_atlas: Arc<dyn Texture>,
    pipeline_layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
}

impl ImGuiRenderer {
    fn new(device: &ash::Device) -> Result<Self, vk::Result> {
        let bindings = [
            vk::DescriptorSetLayoutBinding::builder()
                .binding(0)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::VERTEX)
                .build(),
            vk::DescriptorSetLayoutBinding::builder()
                .binding(1)
                .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::VERTEX)
                .build(),
            vk::DescriptorSetLayoutBinding::builder()
                .binding(2)
                .descriptor_type(vk::DescriptorType::SAMPLER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::FRAGMENT)
                .build(),
            vk::DescriptorSetLayoutBinding::builder()
                .binding(3)
                .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::FRAGMENT)
                .build(),
        ];

        let set_layout_infos = [vk::DescriptorSetLayoutCreateInfo::builder()
            .bindings(&bindings)
            .build()];
        let set_layouts = set_layout_infos
            .iter()
            .map(|info| unsafe { device.create_descriptor_set_layout(info, None) }.unwrap())
            .collect::<Vec<_>>();

        let layout_info = vk::PipelineLayoutCreateInfo::builder()
            .set_layouts(&set_layouts)
            .build();
        let pipeline_layout = unsafe { device.create_pipeline_layout(&layout_info, None) }.unwrap();

        let vs_module = {
            #[allow(clippy::cast_ptr_alignment)]
            let shader_info = vk::ShaderModuleCreateInfo::builder().code(unsafe {
                assert_eq!(shaders::IMGUI_VS.len() % 4, 0);
                std::slice::from_raw_parts(
                    shaders::IMGUI_VS.as_ptr() as *const u32,
                    shaders::IMGUI_VS.len() / 4,
                )
            });
            unsafe { device.create_shader_module(&shader_info, None) }?
        };
        let ps_module = {
            #[allow(clippy::cast_ptr_alignment)]
            let shader_info = vk::ShaderModuleCreateInfo::builder().code(unsafe {
                assert_eq!(shaders::IMGUI_PS.len() % 4, 0);
                std::slice::from_raw_parts(
                    shaders::IMGUI_PS.as_ptr() as *const u32,
                    shaders::IMGUI_PS.len() / 4,
                )
            });
            unsafe { device.create_shader_module(&shader_info, None) }?
        };

        let stages = [
            vk::PipelineShaderStageCreateInfo::builder()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(vs_module)
                .name(std::ffi::CStr::from_bytes_with_nul(b"main\0").unwrap())
                .build(),
            vk::PipelineShaderStageCreateInfo::builder()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(ps_module)
                .name(std::ffi::CStr::from_bytes_with_nul(b"main\0").unwrap())
                .build(),
        ];

        let vertex_input_state = vk::PipelineVertexInputStateCreateInfo::builder();
        let input_assembly_state = vk::PipelineInputAssemblyStateCreateInfo::builder()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        //let viewport_state = vk::PipelineViewportStateCreateInfo::builder()
        //    .viewport_count(1)
        //    .viewports(vk::Viewport::builder().)
        let rasterization_state = vk::PipelineRasterizationStateCreateInfo::builder()
            .depth_clamp_enable(true)
            .rasterizer_discard_enable(true)
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .front_face(vk::FrontFace::CLOCKWISE)
            .depth_bias_enable(false);
        let multisample_state = vk::PipelineMultisampleStateCreateInfo::builder()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1)
            .sample_shading_enable(false)
            .sample_mask(&[!0u32])
            .alpha_to_coverage_enable(false)
            .alpha_to_one_enable(false);
        let depth_stencil_state = vk::PipelineDepthStencilStateCreateInfo::builder()
            .depth_test_enable(false)
            .depth_write_enable(false)
            .depth_compare_op(vk::CompareOp::ALWAYS)
            .depth_bounds_test_enable(false)
            .stencil_test_enable(false);
        let attachments = [vk::PipelineColorBlendAttachmentState::builder()
            .blend_enable(false).build()]; //TODO(max)
        let color_blend_state = vk::PipelineColorBlendStateCreateInfo::builder()
            .logic_op_enable(true)
            .logic_op(vk::LogicOp::SET)
            .attachments(&attachments)
            .blend_constants([1.0, 1.0, 1.0, 1.0]);
        let dynamic_state = vk::PipelineDynamicStateCreateInfo::builder()
            .dynamic_states(&[vk::DynamicState::VIEWPORT]);

     


        let attachments = vk::AttachmentDescription::builder()
            .format(vk::Format::B8G8R8A8_UNORM)
            .samples(vk::SampleCountFlags::TYPE_1)
            .load_op(vk::AttachmentLoadOp::LOAD)
            .store_op(vk::AttachmentStoreOp::STORE)
            .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
            .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
            .initial_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .final_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .build();
        let subpass_begin_info = vk::SubpassBeginInfo::builder()
            .contents(vk::SubpassContents::INLINE)
            .build();

        let subpass_description = vk::SubpassDescription::builder()
            

        let render_pass_create_info = vk::RenderPassCreateInfo::builder()
            .attachments(&[attachments])
            .subpasses()
            .build();
        let render_pass = unsafe { device.create_render_pass(&render_pass_create_info, None) }.unwrap();

        let pipeline_create_info = vk::GraphicsPipelineCreateInfo::builder()
            .stages(&stages)
            .vertex_input_state(&vertex_input_state)
            .input_assembly_state(&input_assembly_state)
            .rasterization_state(&rasterization_state)
            .multisample_state(&multisample_state)
            .depth_stencil_state(&depth_stencil_state)
            .color_blend_state(&color_blend_state)
            .dynamic_state(&dynamic_state)
            .layout(pipeline_layout)
            .render_pass(render_pass)
            .subpass(0)
            .build();

        let pipeline = unsafe {
            device.create_graphics_pipelines(
                vk::PipelineCache::null(),
                &[pipeline_create_info],
                None,
            )
        }
        .unwrap()[0];

        Ok(Self {
            pipeline_layout,
            pipeline,
        })
    }
}

fn record_and_submit_command_buffer<D: DeviceV1_0, F: FnOnce(&D, vk::CommandBuffer)>(
    device: &D,
    command_buffer: vk::CommandBuffer,
    command_buffer_reuse_fence: vk::Fence,
    submit_queue: vk::Queue,
    wait_mask: &[vk::PipelineStageFlags],
    wait_semaphores: &[vk::Semaphore],
    signal_semaphores: &[vk::Semaphore],
    f: F,
) {
    unsafe { device.wait_for_fences(&[command_buffer_reuse_fence], true, std::u64::MAX) }.unwrap();
    unsafe { device.reset_fences(&[command_buffer_reuse_fence]) }.unwrap();
    unsafe {
        device.reset_command_buffer(
            command_buffer,
            vk::CommandBufferResetFlags::RELEASE_RESOURCES,
        )
    }
    .unwrap();

    let command_buffer_begin_info =
        vk::CommandBufferBeginInfo::builder().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
    unsafe { device.begin_command_buffer(command_buffer, &command_buffer_begin_info) }.unwrap();

    f(device, command_buffer);

    unsafe { device.end_command_buffer(command_buffer) }.unwrap();

    let command_buffers = [command_buffer];
    let submit_info = vk::SubmitInfo::builder()
        .wait_semaphores(wait_semaphores)
        .wait_dst_stage_mask(wait_mask)
        .command_buffers(&command_buffers)
        .signal_semaphores(signal_semaphores);

    unsafe {
        device.queue_submit(
            submit_queue,
            &[submit_info.build()],
            command_buffer_reuse_fence,
        )
    }
    .unwrap();
}
fn main() {
    let entry = ash::Entry::new().unwrap();

    let event_loop = winit::event_loop::EventLoop::new();

    let window_width = 1920;
    let window_height = 1080;
    let window = winit::window::WindowBuilder::new()
        .with_title("gpu-allocator vulkan visualization")
        .with_inner_size(winit::dpi::PhysicalSize::new(
            window_width as f64,
            window_height as f64,
        ))
        .with_resizable(false)
        .build(&event_loop)
        .unwrap();

    let (event_send, event_recv) = std::sync::mpsc::sync_channel(1);
    let quit_send = event_loop.create_proxy();

    std::thread::spawn(move || -> Result<(), vk::Result> {
        // Create vulkan instance
        let instance = {
            let app_name = CString::new("gpu-allocator examples vulkan-visualization").unwrap();

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

        let surface =
            unsafe { ash_window::create_surface(&entry, &instance, &window, None) }.unwrap();
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
                            let supports_graphics =
                                info.queue_flags.contains(vk::QueueFlags::GRAPHICS);
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
            let device_extension_names_raw = vec![ash::extensions::khr::Swapchain::name().as_ptr()];
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
            unsafe { surface_loader.get_physical_device_surface_formats(pdevice, surface) }
                .unwrap()[0];
        dbg!(&surface_format);
        let surface_capabilities =
            unsafe { surface_loader.get_physical_device_surface_capabilities(pdevice, surface) }
                .unwrap();
        dbg!(&surface_capabilities);
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

        dbg!(&swapchain_create_info.clone());
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

        let fence_create_info =
            vk::FenceCreateInfo::builder().flags(vk::FenceCreateFlags::SIGNALED);
        let draw_commands_reuse_fence =
            unsafe { device.create_fence(&fence_create_info, None) }.unwrap();
        let setup_commands_reuse_fence =
            unsafe { device.create_fence(&fence_create_info, None) }.unwrap();

        record_and_submit_command_buffer(
            &device,
            setup_command_buffer,
            setup_commands_reuse_fence,
            present_queue,
            &[],
            &[],
            &[],
            |device, cmd| {
                let layout_transition_barriers = vk::ImageMemoryBarrier::builder()
                    .image(depth_image)
                    .dst_access_mask(
                        vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_READ
                            | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE,
                    )
                    .new_layout(vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL)
                    .old_layout(vk::ImageLayout::UNDEFINED)
                    .subresource_range(
                        vk::ImageSubresourceRange::builder()
                            .aspect_mask(vk::ImageAspectFlags::DEPTH)
                            .layer_count(1)
                            .level_count(1)
                            .build(),
                    );

                unsafe {
                    device.cmd_pipeline_barrier(
                        cmd,
                        vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                        vk::PipelineStageFlags::LATE_FRAGMENT_TESTS,
                        vk::DependencyFlags::empty(),
                        &[],
                        &[],
                        &[layout_transition_barriers.build()],
                    )
                };
            },
        );

        let depth_image_view_info = vk::ImageViewCreateInfo::builder()
            .subresource_range(
                vk::ImageSubresourceRange::builder()
                    .aspect_mask(vk::ImageAspectFlags::DEPTH)
                    .level_count(1)
                    .layer_count(1)
                    .build(),
            )
            .image(depth_image)
            .format(depth_image_create_info.format)
            .view_type(vk::ImageViewType::TYPE_2D);
        let depth_image_view =
            unsafe { device.create_image_view(&depth_image_view_info, None) }.unwrap();

        let semaphore_create_info = vk::SemaphoreCreateInfo::default();

        let present_complete_semaphore =
            unsafe { device.create_semaphore(&semaphore_create_info, None) }.unwrap();
        let rendering_complete_semaphore =
            unsafe { device.create_semaphore(&semaphore_create_info, None) }.unwrap();

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

        loop {
            let event = event_recv.recv().unwrap();

            let mut should_quit = false;
            match event {
                winit::event::Event::WindowEvent { event, .. } => match event {
                    winit::event::WindowEvent::KeyboardInput { input, .. } => {
                        if let Some(winit::event::VirtualKeyCode::Escape) = input.virtual_keycode {
                            should_quit = true;
                        }
                    }
                    winit::event::WindowEvent::CloseRequested => {
                        should_quit = true;
                    }
                    _ => {}
                },
                _ => {}
            }

            if should_quit {
                quit_send.send_event(()).unwrap();
                break;
            }
        }

        Ok(())

        // TODO(max): Clean up
    });

    event_loop.run(move |event, _, control_flow| {
        *control_flow = winit::event_loop::ControlFlow::Wait;

        if event == winit::event::Event::UserEvent(()) {
            *control_flow = winit::event_loop::ControlFlow::Exit;
        } else if let Some(event) = event.to_static() {
            let _ = event_send.send(event);
        } else {
            *control_flow = winit::event_loop::ControlFlow::Exit;
        }
    });
}
