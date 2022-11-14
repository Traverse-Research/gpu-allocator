use std::default::Default;
use std::ffi::CString;

use ash::vk;
use gpu_allocator::vulkan::{Allocator, AllocatorCreateDesc};
use raw_window_handle::{HasRawDisplayHandle, HasRawWindowHandle};

mod imgui_renderer;
use imgui_renderer::{handle_imgui_event, ImGuiRenderer};

mod helper;
use helper::record_and_submit_command_buffer;

fn main() -> ash::prelude::VkResult<()> {
    let entry = unsafe { ash::Entry::load() }.unwrap();

    let event_loop = winit::event_loop::EventLoop::new();

    let window_width = 1920;
    let window_height = 1080;
    let window = winit::window::WindowBuilder::new()
        .with_title("gpu-allocator Vulkan visualization")
        .with_inner_size(winit::dpi::PhysicalSize::new(
            window_width as f64,
            window_height as f64,
        ))
        .with_resizable(false)
        .build(&event_loop)
        .unwrap();

    // Create Vulkan instance
    let instance = {
        let app_name = CString::new("gpu-allocator examples vulkan-visualization").unwrap();

        let appinfo = vk::ApplicationInfo::builder()
            .application_name(&app_name)
            .application_version(0)
            .engine_name(&app_name)
            .engine_version(0)
            .api_version(vk::make_api_version(0, 1, 0, 0));

        let layer_names: &[CString] = &[CString::new("VK_LAYER_KHRONOS_validation").unwrap()];
        let layers_names_raw: Vec<*const i8> = layer_names
            .iter()
            .map(|raw_name| raw_name.as_ptr())
            .collect();

        let surface_extensions =
            ash_window::enumerate_required_extensions(event_loop.raw_display_handle()).unwrap();

        let create_info = vk::InstanceCreateInfo::builder()
            .application_info(&appinfo)
            .enabled_layer_names(&layers_names_raw)
            .enabled_extension_names(surface_extensions);

        unsafe {
            entry
                .create_instance(&create_info, None)
                .expect("Instance creation error")
        }
    };

    let surface = unsafe {
        ash_window::create_surface(
            &entry,
            &instance,
            window.raw_display_handle(),
            window.raw_window_handle(),
            None,
        )
    }
    .unwrap();
    let surface_loader = ash::extensions::khr::Surface::new(&entry, &instance);

    // Look for Vulkan physical device
    let (pdevice, queue_family_index) = {
        let pdevices = unsafe {
            instance
                .enumerate_physical_devices()
                .expect("Physical device error")
        };
        pdevices
            .iter()
            .find_map(|pdevice| {
                unsafe { instance.get_physical_device_queue_family_properties(*pdevice) }
                    .iter()
                    .enumerate()
                    .find_map(|(index, &info)| {
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
            })
            .expect("Couldn't find suitable device.")
    };

    // Create Vulkan device
    let device = {
        let device_extension_names_raw = [ash::extensions::khr::Swapchain::name().as_ptr()];
        let features = vk::PhysicalDeviceFeatures {
            shader_clip_distance: 1,
            ..Default::default()
        };
        let priorities = [1.0];

        let queue_info = vk::DeviceQueueCreateInfo::builder()
            .queue_family_index(queue_family_index as u32)
            .queue_priorities(&priorities);

        let create_info = vk::DeviceCreateInfo::builder()
            .queue_create_infos(std::slice::from_ref(&queue_info))
            .enabled_extension_names(&device_extension_names_raw)
            .enabled_features(&features);

        unsafe { instance.create_device(pdevice, &create_info, None) }.unwrap()
    };

    let present_queue = unsafe { device.get_device_queue(queue_family_index as u32, 0) };

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
        u32::MAX => vk::Extent2D {
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
    let command_pool = unsafe { device.create_command_pool(&pool_create_info, None) }.unwrap();

    let command_buffer_allocate_info = vk::CommandBufferAllocateInfo::builder()
        .command_buffer_count(2)
        .command_pool(command_pool)
        .level(vk::CommandBufferLevel::PRIMARY);

    let command_buffers =
        unsafe { device.allocate_command_buffers(&command_buffer_allocate_info) }.unwrap();
    let setup_command_buffer = command_buffers[0];
    let draw_command_buffer = command_buffers[1];

    let present_images = unsafe { swapchain_loader.get_swapchain_images(swapchain) }.unwrap();
    let mut present_image_views = present_images
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

    // Setting up the allocator
    let mut allocator = Some(
        Allocator::new(&AllocatorCreateDesc {
            instance: instance.clone(),
            device: device.clone(),
            physical_device: pdevice,
            debug_settings: Default::default(),
            buffer_device_address: false,
        })
        .unwrap(),
    );

    let fence_create_info = vk::FenceCreateInfo::builder().flags(vk::FenceCreateFlags::SIGNALED);
    let draw_commands_reuse_fence =
        unsafe { device.create_fence(&fence_create_info, None) }.unwrap();
    let setup_commands_reuse_fence =
        unsafe { device.create_fence(&fence_create_info, None) }.unwrap();

    let semaphore_create_info = vk::SemaphoreCreateInfo::default();

    let present_complete_semaphore =
        unsafe { device.create_semaphore(&semaphore_create_info, None) }.unwrap();
    let rendering_complete_semaphore =
        unsafe { device.create_semaphore(&semaphore_create_info, None) }.unwrap();

    let mut imgui = imgui::Context::create();
    imgui.io_mut().display_size = [window_width as f32, window_height as f32];

    let descriptor_pool = {
        let pool_sizes = [
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::UNIFORM_BUFFER,
                descriptor_count: 1,
            },
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::SAMPLED_IMAGE,
                descriptor_count: 1,
            },
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::SAMPLER,
                descriptor_count: 1,
            },
        ];
        let create_info = vk::DescriptorPoolCreateInfo::builder()
            .max_sets(1)
            .pool_sizes(&pool_sizes);
        unsafe { device.create_descriptor_pool(&create_info, None) }?
    };

    let mut imgui_renderer = Some(ImGuiRenderer::new(
        &mut imgui,
        &device,
        descriptor_pool,
        surface_format.format,
        allocator.as_mut().unwrap(),
        setup_command_buffer,
        setup_commands_reuse_fence,
        present_queue,
    )?);

    let mut framebuffers = present_image_views
        .iter()
        .map(|&view| {
            let create_info = vk::FramebufferCreateInfo::builder()
                .render_pass(imgui_renderer.as_ref().unwrap().render_pass)
                .attachments(std::slice::from_ref(&view))
                .width(window_width)
                .height(window_height)
                .layers(1);

            unsafe { device.create_framebuffer(&create_info, None) }.unwrap()
        })
        .collect::<Vec<_>>();

    let mut visualizer = Some(gpu_allocator::vulkan::AllocatorVisualizer::new());

    event_loop.run(move |event, _, control_flow| {
        *control_flow = winit::event_loop::ControlFlow::Wait;

        handle_imgui_event(imgui.io_mut(), &window, &event);

        let mut ready_for_rendering = false;
        match event {
            winit::event::Event::WindowEvent { event, .. } => match event {
                winit::event::WindowEvent::CloseRequested
                | winit::event::WindowEvent::KeyboardInput {
                    input:
                        winit::event::KeyboardInput {
                            virtual_keycode: Some(winit::event::VirtualKeyCode::Escape),
                            ..
                        },
                    ..
                } => {
                    *control_flow = winit::event_loop::ControlFlow::Exit;
                }
                _ => {}
            },
            winit::event::Event::MainEventsCleared => ready_for_rendering = true,
            _ => {}
        }

        if ready_for_rendering {
            let (present_index, _) = unsafe {
                swapchain_loader.acquire_next_image(
                    swapchain,
                    u64::MAX,
                    present_complete_semaphore,
                    vk::Fence::null(),
                )
            }
            .unwrap();

            // Start ImGui frame
            let ui = imgui.frame();

            // Submit visualizer ImGui commands
            visualizer
                .as_mut()
                .unwrap()
                .render(allocator.as_ref().unwrap(), &ui, None);

            // Finish ImGui Frame
            let imgui_draw_data = ui.render();

            record_and_submit_command_buffer(
                &device,
                draw_command_buffer,
                draw_commands_reuse_fence,
                present_queue,
                &[vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT],
                &[present_complete_semaphore],
                &[rendering_complete_semaphore],
                |device, cmd| {
                    // Render ImGui to swapchain image
                    imgui_renderer.as_mut().unwrap().render(
                        imgui_draw_data,
                        device,
                        window_width,
                        window_height,
                        framebuffers[present_index as usize],
                        cmd,
                    );

                    // Transition swapchain image to present state
                    let image_barriers = vk::ImageMemoryBarrier::builder()
                        .src_access_mask(
                            vk::AccessFlags::COLOR_ATTACHMENT_READ
                                | vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
                        )
                        .old_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                        .new_layout(vk::ImageLayout::PRESENT_SRC_KHR)
                        .image(present_images[present_index as usize])
                        .subresource_range(vk::ImageSubresourceRange {
                            aspect_mask: vk::ImageAspectFlags::COLOR,
                            base_mip_level: 0,
                            level_count: vk::REMAINING_MIP_LEVELS,
                            base_array_layer: 0,
                            layer_count: vk::REMAINING_ARRAY_LAYERS,
                        });
                    unsafe {
                        device.cmd_pipeline_barrier(
                            cmd,
                            vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                            vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                            vk::DependencyFlags::empty(),
                            &[],
                            &[],
                            std::slice::from_ref(&image_barriers),
                        )
                    };
                },
            );

            let present_create_info = vk::PresentInfoKHR::builder()
                .wait_semaphores(std::slice::from_ref(&rendering_complete_semaphore))
                .swapchains(std::slice::from_ref(&swapchain))
                .image_indices(std::slice::from_ref(&present_index));

            unsafe { swapchain_loader.queue_present(present_queue, &present_create_info) }.unwrap();
        } else if *control_flow == winit::event_loop::ControlFlow::Exit {
            unsafe { device.queue_wait_idle(present_queue) }.unwrap();

            visualizer.take();

            for fb in framebuffers.drain(..) {
                unsafe { device.destroy_framebuffer(fb, None) };
            }

            let mut allocator = allocator.take().unwrap();

            imgui_renderer
                .take()
                .unwrap()
                .destroy(&device, &mut allocator);

            unsafe { device.destroy_descriptor_pool(descriptor_pool, None) };
            unsafe { device.destroy_semaphore(rendering_complete_semaphore, None) };
            unsafe { device.destroy_semaphore(present_complete_semaphore, None) };
            unsafe { device.destroy_fence(setup_commands_reuse_fence, None) };
            unsafe { device.destroy_fence(draw_commands_reuse_fence, None) };
            drop(allocator);
            for view in present_image_views.drain(..) {
                unsafe { device.destroy_image_view(view, None) };
            }
            unsafe { device.free_command_buffers(command_pool, &command_buffers) };
            unsafe { device.destroy_command_pool(command_pool, None) };
            unsafe { swapchain_loader.destroy_swapchain(swapchain, None) };
            unsafe { device.destroy_device(None) };
            unsafe {
                surface_loader.destroy_surface(surface, None);
            }
            unsafe { instance.destroy_instance(None) };
        }
    });
}
