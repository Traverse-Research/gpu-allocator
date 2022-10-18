use ash::vk;

use crate::helper::record_and_submit_command_buffer;
use gpu_allocator::vulkan::{Allocation, AllocationCreateDesc, Allocator};
use gpu_allocator::MemoryLocation;

#[repr(C)]
#[derive(Clone, Copy)]
struct ImGuiCBuffer {
    scale: [f32; 2],
    translation: [f32; 2],
}
pub struct ImGuiRenderer {
    sampler: vk::Sampler,

    vb_capacity: u64,
    ib_capacity: u64,
    vb_allocation: Allocation,
    ib_allocation: Allocation,
    vertex_buffer: vk::Buffer,
    index_buffer: vk::Buffer,

    cb_allocation: Allocation,
    constant_buffer: vk::Buffer,

    font_image: vk::Image,
    font_image_memory: Allocation,
    font_image_view: vk::ImageView,

    descriptor_sets: Vec<vk::DescriptorSet>,

    vs_module: vk::ShaderModule,
    ps_module: vk::ShaderModule,
    descriptor_set_layouts: Vec<vk::DescriptorSetLayout>,
    pipeline_layout: vk::PipelineLayout,
    pub(crate) render_pass: vk::RenderPass,
    pipeline: vk::Pipeline,
}

impl ImGuiRenderer {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        imgui: &mut imgui::Context,
        device: &ash::Device,
        descriptor_pool: vk::DescriptorPool,
        render_target_format: vk::Format,
        allocator: &mut Allocator,
        cmd: vk::CommandBuffer,
        cmd_reuse_fence: vk::Fence,
        queue: vk::Queue,
    ) -> Result<Self, vk::Result> {
        let (pipeline_layout, descriptor_set_layouts) = {
            let bindings = [
                vk::DescriptorSetLayoutBinding {
                    binding: 0,
                    descriptor_type: vk::DescriptorType::UNIFORM_BUFFER,
                    descriptor_count: 1,
                    stage_flags: vk::ShaderStageFlags::VERTEX,
                    p_immutable_samplers: std::ptr::null(),
                },
                vk::DescriptorSetLayoutBinding {
                    binding: 1,
                    descriptor_type: vk::DescriptorType::SAMPLER,
                    descriptor_count: 1,
                    stage_flags: vk::ShaderStageFlags::FRAGMENT,
                    p_immutable_samplers: std::ptr::null(),
                },
                vk::DescriptorSetLayoutBinding {
                    binding: 2,
                    descriptor_type: vk::DescriptorType::SAMPLED_IMAGE,
                    descriptor_count: 1,
                    stage_flags: vk::ShaderStageFlags::FRAGMENT,
                    p_immutable_samplers: std::ptr::null(),
                },
            ];

            let set_layout_infos =
                [vk::DescriptorSetLayoutCreateInfo::builder().bindings(&bindings)];
            let set_layouts = set_layout_infos
                .iter()
                .map(|info| unsafe { device.create_descriptor_set_layout(info, None) })
                .collect::<Result<Vec<_>, vk::Result>>()?;

            let layout_info = vk::PipelineLayoutCreateInfo::builder().set_layouts(&set_layouts);
            let pipeline_layout = unsafe { device.create_pipeline_layout(&layout_info, None) }?;

            (pipeline_layout, set_layouts)
        };

        let render_pass = {
            let attachments = vk::AttachmentDescription::builder()
                .format(render_target_format)
                .samples(vk::SampleCountFlags::TYPE_1)
                .load_op(vk::AttachmentLoadOp::CLEAR)
                .store_op(vk::AttachmentStoreOp::STORE)
                .final_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);

            let subpass_attachment = vk::AttachmentReference::builder()
                .attachment(0)
                .layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
            let subpass_description = vk::SubpassDescription::builder()
                .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
                .color_attachments(std::slice::from_ref(&subpass_attachment));

            let dependencies = vk::SubpassDependency::builder()
                .src_subpass(vk::SUBPASS_EXTERNAL)
                .src_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
                .dst_access_mask(
                    vk::AccessFlags::COLOR_ATTACHMENT_READ
                        | vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
                )
                .dst_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT);

            let render_pass_create_info = vk::RenderPassCreateInfo::builder()
                .attachments(std::slice::from_ref(&attachments))
                .subpasses(std::slice::from_ref(&subpass_description))
                .dependencies(std::slice::from_ref(&dependencies));
            unsafe { device.create_render_pass(&render_pass_create_info, None) }.unwrap()
        };

        let vs_module = {
            let vs = include_bytes!("./spirv/imgui.vs.spv");

            #[allow(clippy::cast_ptr_alignment)]
            let shader_info = vk::ShaderModuleCreateInfo::builder().code(unsafe {
                assert_eq!(vs.len() % 4, 0);
                std::slice::from_raw_parts(vs.as_ptr().cast(), vs.len() / 4)
            });
            unsafe { device.create_shader_module(&shader_info, None) }?
        };
        let ps_module = {
            let ps = include_bytes!("./spirv/imgui.ps.spv");

            #[allow(clippy::cast_ptr_alignment)]
            let shader_info = vk::ShaderModuleCreateInfo::builder().code(unsafe {
                assert_eq!(ps.len() % 4, 0);
                std::slice::from_raw_parts(ps.as_ptr().cast(), ps.len() / 4)
            });
            unsafe { device.create_shader_module(&shader_info, None) }?
        };

        let pipeline = {
            let vertex_stage = vk::PipelineShaderStageCreateInfo::builder()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(vs_module)
                .name(std::ffi::CStr::from_bytes_with_nul(b"main\0").unwrap());
            let fragment_stage = vk::PipelineShaderStageCreateInfo::builder()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(ps_module)
                .name(std::ffi::CStr::from_bytes_with_nul(b"main\0").unwrap());
            let stages = [vertex_stage.build(), fragment_stage.build()];

            let vertex_binding_descriptions = [vk::VertexInputBindingDescription {
                binding: 0,
                stride: std::mem::size_of::<imgui::DrawVert>() as u32,
                input_rate: vk::VertexInputRate::VERTEX,
            }];
            let vertex_attribute_descriptions = [
                vk::VertexInputAttributeDescription {
                    location: 0,
                    binding: 0,
                    format: vk::Format::R32G32_SFLOAT,
                    offset: 0,
                },
                vk::VertexInputAttributeDescription {
                    location: 1,
                    binding: 0,
                    format: vk::Format::R32G32_SFLOAT,
                    offset: 8,
                },
                vk::VertexInputAttributeDescription {
                    location: 2,
                    binding: 0,
                    format: vk::Format::R8G8B8A8_UNORM,
                    offset: 16,
                },
            ];
            let vertex_input_state = vk::PipelineVertexInputStateCreateInfo::builder()
                .vertex_binding_descriptions(&vertex_binding_descriptions)
                .vertex_attribute_descriptions(&vertex_attribute_descriptions);
            let input_assembly_state = vk::PipelineInputAssemblyStateCreateInfo::builder()
                .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
            let viewport_state = vk::PipelineViewportStateCreateInfo::builder()
                .viewport_count(1)
                .scissor_count(1);
            let rasterization_state = vk::PipelineRasterizationStateCreateInfo::builder()
                .polygon_mode(vk::PolygonMode::FILL)
                .cull_mode(vk::CullModeFlags::NONE)
                .front_face(vk::FrontFace::CLOCKWISE)
                .depth_bias_enable(false)
                .line_width(1.0);
            let multisample_state = vk::PipelineMultisampleStateCreateInfo::builder()
                .rasterization_samples(vk::SampleCountFlags::TYPE_1);
            let noop_stencil_state = vk::StencilOpState {
                fail_op: vk::StencilOp::KEEP,
                pass_op: vk::StencilOp::KEEP,
                depth_fail_op: vk::StencilOp::KEEP,
                compare_op: vk::CompareOp::ALWAYS,
                ..Default::default()
            };
            let depth_stencil_state = vk::PipelineDepthStencilStateCreateInfo::builder()
                .depth_test_enable(false)
                .depth_write_enable(false)
                .depth_compare_op(vk::CompareOp::ALWAYS)
                .depth_bounds_test_enable(false)
                .stencil_test_enable(false)
                .front(noop_stencil_state)
                .back(noop_stencil_state)
                .max_depth_bounds(1.0);
            let attachments = vk::PipelineColorBlendAttachmentState::builder()
                .blend_enable(true)
                .src_color_blend_factor(vk::BlendFactor::SRC_ALPHA)
                .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
                .color_blend_op(vk::BlendOp::ADD)
                .src_alpha_blend_factor(vk::BlendFactor::ZERO)
                .dst_alpha_blend_factor(vk::BlendFactor::ZERO)
                .alpha_blend_op(vk::BlendOp::ADD)
                .color_write_mask({
                    vk::ColorComponentFlags::R
                        | vk::ColorComponentFlags::G
                        | vk::ColorComponentFlags::B
                        | vk::ColorComponentFlags::A
                });
            let color_blend_state = vk::PipelineColorBlendStateCreateInfo::builder()
                .logic_op(vk::LogicOp::CLEAR)
                .attachments(std::slice::from_ref(&attachments));
            let dynamic_state = vk::PipelineDynamicStateCreateInfo::builder()
                .dynamic_states(&[vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR]);

            let pipeline_create_info = vk::GraphicsPipelineCreateInfo::builder()
                .stages(&stages)
                .vertex_input_state(&vertex_input_state)
                .input_assembly_state(&input_assembly_state)
                .viewport_state(&viewport_state)
                .rasterization_state(&rasterization_state)
                .multisample_state(&multisample_state)
                .depth_stencil_state(&depth_stencil_state)
                .color_blend_state(&color_blend_state)
                .dynamic_state(&dynamic_state)
                .layout(pipeline_layout)
                .render_pass(render_pass)
                .subpass(0);

            unsafe {
                device.create_graphics_pipelines(
                    vk::PipelineCache::null(),
                    std::slice::from_ref(&pipeline_create_info),
                    None,
                )
            }
            .unwrap()[0]
        };

        let (font_image, font_image_memory, font_image_view) = {
            let mut fonts = imgui.fonts();
            let font_atlas = fonts.build_rgba32_texture();

            // Create image
            let image_usage = vk::ImageUsageFlags::SAMPLED
                | vk::ImageUsageFlags::TRANSFER_DST
                | vk::ImageUsageFlags::TRANSFER_SRC;
            let create_info = vk::ImageCreateInfo::builder()
                .image_type(vk::ImageType::TYPE_2D)
                .format(vk::Format::R8G8B8A8_UNORM)
                .extent(vk::Extent3D {
                    width: font_atlas.width,
                    height: font_atlas.height,
                    depth: 1,
                })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(image_usage)
                .initial_layout(vk::ImageLayout::UNDEFINED);
            let image = unsafe { device.create_image(&create_info, None) }?;

            // Allocate and bind memory to image
            let requirements = unsafe { device.get_image_memory_requirements(image) };
            let allocation = allocator
                .allocate(&AllocationCreateDesc {
                    name: "ImGui font image",
                    requirements,
                    location: MemoryLocation::GpuOnly,
                    linear: false,
                })
                .unwrap();
            unsafe { device.bind_image_memory(image, allocation.memory(), allocation.offset()) }
                .unwrap();

            // Create image view
            let view_create_info = vk::ImageViewCreateInfo::builder()
                .image(image)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(vk::Format::R8G8B8A8_UNORM)
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
                });
            let image_view = unsafe { device.create_image_view(&view_create_info, None) }?;

            // Create upload buffer
            let (upload_buffer, mut upload_buffer_memory) = {
                let create_info = vk::BufferCreateInfo::builder()
                    .size((font_atlas.width * font_atlas.height * 4) as u64)
                    .usage(vk::BufferUsageFlags::TRANSFER_SRC);
                let buffer = unsafe { device.create_buffer(&create_info, None) }?;

                let requirements = unsafe { device.get_buffer_memory_requirements(buffer) };

                let buffer_memory = allocator
                    .allocate(&AllocationCreateDesc {
                        name: "ImGui font image upload buffer",
                        requirements,
                        location: MemoryLocation::CpuToGpu,
                        linear: true,
                    })
                    .unwrap();

                unsafe {
                    device.bind_buffer_memory(
                        buffer,
                        buffer_memory.memory(),
                        buffer_memory.offset(),
                    )
                }?;

                (buffer, buffer_memory)
            };

            // Copy font data to upload buffer
            let mut slab = upload_buffer_memory.as_mapped_slab().unwrap();
            presser::copy_from_slice_to_offset(font_atlas.data, &mut slab, 0).unwrap();

            // Copy upload buffer to image
            record_and_submit_command_buffer(
                device,
                cmd,
                cmd_reuse_fence,
                queue,
                &[],
                &[],
                &[],
                |device, cmd| {
                    {
                        let layout_transition_barriers = vk::ImageMemoryBarrier::builder()
                            .image(image)
                            .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                            .old_layout(vk::ImageLayout::UNDEFINED)
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
                                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                                vk::PipelineStageFlags::TRANSFER,
                                vk::DependencyFlags::empty(),
                                &[],
                                &[],
                                std::slice::from_ref(&layout_transition_barriers),
                            )
                        };
                    }

                    let regions = vk::BufferImageCopy::builder()
                        .buffer_offset(0)
                        .buffer_row_length(font_atlas.width)
                        .buffer_image_height(font_atlas.height)
                        .image_subresource(vk::ImageSubresourceLayers {
                            aspect_mask: vk::ImageAspectFlags::COLOR,
                            mip_level: 0,
                            base_array_layer: 0,
                            layer_count: 1,
                        })
                        .image_offset(vk::Offset3D { x: 0, y: 0, z: 0 })
                        .image_extent(vk::Extent3D {
                            width: font_atlas.width,
                            height: font_atlas.height,
                            depth: 1,
                        });
                    unsafe {
                        device.cmd_copy_buffer_to_image(
                            cmd,
                            upload_buffer,
                            image,
                            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                            std::slice::from_ref(&regions),
                        )
                    };

                    {
                        let layout_transition_barriers = vk::ImageMemoryBarrier::builder()
                            .image(image)
                            .dst_access_mask(vk::AccessFlags::SHADER_READ)
                            .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
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
                                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                                vk::PipelineStageFlags::FRAGMENT_SHADER,
                                vk::DependencyFlags::empty(),
                                &[],
                                &[],
                                std::slice::from_ref(&layout_transition_barriers),
                            )
                        };
                    }
                },
            );

            unsafe { device.queue_wait_idle(queue) }?;

            // Free upload buffer
            unsafe { device.destroy_buffer(upload_buffer, None) };
            allocator.free(upload_buffer_memory).unwrap();

            (image, allocation, image_view)
        };

        let sampler = {
            let create_info = vk::SamplerCreateInfo::builder()
                .mag_filter(vk::Filter::NEAREST)
                .min_filter(vk::Filter::NEAREST)
                .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
                .address_mode_u(vk::SamplerAddressMode::REPEAT)
                .address_mode_v(vk::SamplerAddressMode::REPEAT)
                .address_mode_w(vk::SamplerAddressMode::REPEAT)
                .mip_lod_bias(0.0)
                .anisotropy_enable(false)
                .compare_enable(false)
                .unnormalized_coordinates(false);
            unsafe { device.create_sampler(&create_info, None) }?
        };

        let (vertex_buffer, vb_allocation, vb_capacity) = {
            let capacity = 1024 * 1024;

            let create_info = vk::BufferCreateInfo::builder()
                .size(capacity)
                .usage(vk::BufferUsageFlags::VERTEX_BUFFER)
                .sharing_mode(vk::SharingMode::EXCLUSIVE);

            let buffer = unsafe { device.create_buffer(&create_info, None) }?;

            let requirements = unsafe { device.get_buffer_memory_requirements(buffer) };

            let allocation = allocator
                .allocate(&AllocationCreateDesc {
                    name: "ImGui Vertex buffer",
                    requirements,
                    location: MemoryLocation::CpuToGpu,
                    linear: true,
                })
                .unwrap();

            unsafe { device.bind_buffer_memory(buffer, allocation.memory(), allocation.offset()) }?;

            (buffer, allocation, capacity)
        };
        let (index_buffer, ib_allocation, ib_capacity) = {
            let capacity = 1024 * 1024;

            let create_info = vk::BufferCreateInfo::builder()
                .size(capacity)
                .usage(vk::BufferUsageFlags::INDEX_BUFFER)
                .sharing_mode(vk::SharingMode::EXCLUSIVE);

            let buffer = unsafe { device.create_buffer(&create_info, None) }?;

            let requirements = unsafe { device.get_buffer_memory_requirements(buffer) };

            let allocation = allocator
                .allocate(&AllocationCreateDesc {
                    name: "ImGui Index buffer",
                    requirements,
                    location: MemoryLocation::CpuToGpu,
                    linear: true,
                })
                .unwrap();

            unsafe { device.bind_buffer_memory(buffer, allocation.memory(), allocation.offset()) }?;

            (buffer, allocation, capacity)
        };
        let (constant_buffer, cb_allocation) = {
            let create_info = vk::BufferCreateInfo::builder()
                .size(std::mem::size_of::<ImGuiCBuffer>() as u64)
                .usage(vk::BufferUsageFlags::UNIFORM_BUFFER)
                .sharing_mode(vk::SharingMode::EXCLUSIVE);

            let buffer = unsafe { device.create_buffer(&create_info, None) }?;

            let requirements = unsafe { device.get_buffer_memory_requirements(buffer) };

            let allocation = allocator
                .allocate(&AllocationCreateDesc {
                    name: "ImGui Constant buffer",
                    requirements,
                    location: MemoryLocation::CpuToGpu,
                    linear: true,
                })
                .unwrap();

            unsafe { device.bind_buffer_memory(buffer, allocation.memory(), allocation.offset()) }?;

            (buffer, allocation)
        };

        let descriptor_sets = {
            let alloc_info = vk::DescriptorSetAllocateInfo::builder()
                .descriptor_pool(descriptor_pool)
                .set_layouts(&descriptor_set_layouts);
            let descriptor_sets = unsafe { device.allocate_descriptor_sets(&alloc_info) }?;

            let buffer_info = vk::DescriptorBufferInfo::builder()
                .buffer(constant_buffer)
                .offset(0)
                .range(std::mem::size_of::<ImGuiCBuffer>() as u64);
            let uniform_buffer = vk::WriteDescriptorSet::builder()
                .dst_set(descriptor_sets[0])
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
                .buffer_info(std::slice::from_ref(&buffer_info));

            let image_info = vk::DescriptorImageInfo::builder().sampler(sampler);
            let sampler = vk::WriteDescriptorSet::builder()
                .dst_set(descriptor_sets[0])
                .dst_binding(1)
                .descriptor_type(vk::DescriptorType::SAMPLER)
                .image_info(std::slice::from_ref(&image_info));

            let image_info = vk::DescriptorImageInfo::builder()
                .image_view(font_image_view)
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
            let sampled_image = vk::WriteDescriptorSet::builder()
                .dst_set(descriptor_sets[0])
                .dst_binding(2)
                .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                .image_info(std::slice::from_ref(&image_info));

            unsafe {
                device.update_descriptor_sets(
                    &[
                        uniform_buffer.build(),
                        sampler.build(),
                        sampled_image.build(),
                    ],
                    &[],
                )
            };
            descriptor_sets
        };

        Ok(Self {
            sampler,

            vb_capacity,
            ib_capacity,
            vb_allocation,
            ib_allocation,
            vertex_buffer,
            index_buffer,
            cb_allocation,
            constant_buffer,

            font_image,
            font_image_memory,
            font_image_view,

            descriptor_sets,

            vs_module,
            ps_module,
            descriptor_set_layouts,
            pipeline_layout,
            render_pass,
            pipeline,
        })
    }

    pub(crate) fn render(
        &mut self,
        imgui_draw_data: &imgui::DrawData,
        device: &ash::Device,
        window_width: u32,
        window_height: u32,
        framebuffer: vk::Framebuffer,
        cmd: vk::CommandBuffer,
    ) {
        // Update constant buffer
        {
            let left = imgui_draw_data.display_pos[0];
            let right = imgui_draw_data.display_pos[0] + imgui_draw_data.display_size[0];
            let top = imgui_draw_data.display_pos[1];
            let bottom = imgui_draw_data.display_pos[1] + imgui_draw_data.display_size[1];

            let cbuffer_data = ImGuiCBuffer {
                scale: [(2.0 / (right - left)), (2.0 / (bottom - top))],
                translation: [
                    (right + left) / (left - right),
                    (top + bottom) / (top - bottom),
                ],
            };

            let copy_record = presser::copy_to_offset(
                &cbuffer_data,
                &mut self.cb_allocation.as_mapped_slab().unwrap(),
                0,
            )
            .unwrap();
            assert_eq!(copy_record.copy_start_offset, 0);
        }

        let render_pass_begin_info = vk::RenderPassBeginInfo::builder()
            .render_pass(self.render_pass)
            .framebuffer(framebuffer)
            .render_area(vk::Rect2D {
                offset: vk::Offset2D { x: 0, y: 0 },
                extent: vk::Extent2D {
                    width: window_width,
                    height: window_height,
                },
            })
            .clear_values(&[vk::ClearValue {
                color: vk::ClearColorValue {
                    float32: [1.0, 0.5, 1.0, 0.0],
                },
            }]);
        unsafe {
            device.cmd_begin_render_pass(cmd, &render_pass_begin_info, vk::SubpassContents::INLINE)
        };

        unsafe { device.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, self.pipeline) };

        let viewport = vk::Viewport::builder()
            .x(0.0)
            .y(0.0)
            .width(window_width as f32)
            .height(window_height as f32);
        unsafe { device.cmd_set_viewport(cmd, 0, std::slice::from_ref(&viewport)) };
        {
            let scissor_rect = vk::Rect2D {
                offset: vk::Offset2D { x: 0, y: 0 },
                extent: vk::Extent2D {
                    width: window_width,
                    height: window_height,
                },
            };
            unsafe { device.cmd_set_scissor(cmd, 0, &[scissor_rect]) };
        }

        unsafe {
            device.cmd_bind_descriptor_sets(
                cmd,
                vk::PipelineBindPoint::GRAPHICS,
                self.pipeline_layout,
                0,
                &self.descriptor_sets,
                &[],
            )
        };

        let (vtx_count, idx_count) =
            imgui_draw_data
                .draw_lists()
                .fold((0, 0), |(vtx_count, idx_count), draw_list| {
                    (
                        vtx_count + draw_list.vtx_buffer().len(),
                        idx_count + draw_list.idx_buffer().len(),
                    )
                });

        let vtx_size = (vtx_count * std::mem::size_of::<imgui::DrawVert>()) as u64;
        if vtx_size > self.vb_capacity {
            // reallocate vertex buffer
            todo!();
        }
        let idx_size = (idx_count * std::mem::size_of::<imgui::DrawIdx>()) as u64;
        if idx_size > self.ib_capacity {
            // reallocate index buffer
            todo!();
        }

        let mut vb_offset = 0;
        let mut ib_offset = 0;

        let mut vb_slab = self.vb_allocation.as_mapped_slab().unwrap();
        let mut ib_slab = self.ib_allocation.as_mapped_slab().unwrap();

        for draw_list in imgui_draw_data.draw_lists() {
            {
                let vertices = draw_list.vtx_buffer();
                let copy_record =
                    presser::copy_from_slice_to_offset(vertices, &mut vb_slab, vb_offset).unwrap();
                vb_offset = copy_record.copy_end_offset_padded;

                unsafe {
                    device.cmd_bind_vertex_buffers(
                        cmd,
                        0,
                        &[self.vertex_buffer],
                        &[copy_record.copy_start_offset as _],
                    )
                };
            }

            {
                let indices = draw_list.idx_buffer();
                let copy_record =
                    presser::copy_from_slice_to_offset(indices, &mut ib_slab, ib_offset).unwrap();
                ib_offset = copy_record.copy_end_offset_padded;

                unsafe {
                    device.cmd_bind_index_buffer(
                        cmd,
                        self.index_buffer,
                        copy_record.copy_start_offset as _,
                        vk::IndexType::UINT16,
                    )
                };
            }

            for command in draw_list.commands() {
                match command {
                    imgui::DrawCmd::Elements { count, cmd_params } => {
                        let scissor_rect = vk::Rect2D {
                            offset: vk::Offset2D {
                                x: cmd_params.clip_rect[0] as i32,
                                y: cmd_params.clip_rect[1] as i32,
                            },
                            extent: vk::Extent2D {
                                width: (cmd_params.clip_rect[2] - cmd_params.clip_rect[0]) as u32,
                                height: (cmd_params.clip_rect[3] - cmd_params.clip_rect[1]) as u32,
                            },
                        };
                        unsafe { device.cmd_set_scissor(cmd, 0, &[scissor_rect]) };

                        unsafe {
                            device.cmd_draw_indexed(
                                cmd,
                                count as u32,
                                1,
                                cmd_params.idx_offset as u32,
                                cmd_params.vtx_offset as i32,
                                0,
                            )
                        };
                    }
                    _ => todo!(),
                }
            }
        }

        unsafe { device.cmd_end_render_pass(cmd) };
    }

    pub(crate) fn destroy(self, device: &ash::Device, allocator: &mut Allocator) {
        unsafe { device.destroy_buffer(self.constant_buffer, None) };
        allocator.free(self.cb_allocation).unwrap();

        unsafe { device.destroy_buffer(self.index_buffer, None) };
        allocator.free(self.ib_allocation).unwrap();

        unsafe { device.destroy_buffer(self.vertex_buffer, None) };
        allocator.free(self.vb_allocation).unwrap();

        unsafe {
            device.destroy_sampler(self.sampler, None);
        }
        unsafe {
            device.destroy_image_view(self.font_image_view, None);
        }
        unsafe {
            device.destroy_image(self.font_image, None);
        }
        allocator.free(self.font_image_memory).unwrap();

        unsafe { device.destroy_shader_module(self.ps_module, None) };
        unsafe { device.destroy_shader_module(self.vs_module, None) };

        unsafe { device.destroy_pipeline(self.pipeline, None) };

        unsafe { device.destroy_render_pass(self.render_pass, None) };

        unsafe {
            device.destroy_pipeline_layout(self.pipeline_layout, None);
        }

        for &layout in self.descriptor_set_layouts.iter() {
            unsafe { device.destroy_descriptor_set_layout(layout, None) };
        }
    }
}
