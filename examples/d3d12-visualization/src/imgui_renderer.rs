mod all_dxgi {
    pub use winapi::shared::{
        dxgi::*, dxgi1_2::*, dxgi1_3::*, dxgi1_4::*, dxgi1_6::*, dxgiformat::*, dxgitype::*,
    };
}
use winapi::um::d3d12::*;
use winapi::um::d3dcommon::*;
use winapi::Interface;

use winapi::shared::winerror::FAILED;

use gpu_allocator::d3d12::{Allocation, AllocationCreateDesc, Allocator};
use gpu_allocator::MemoryLocation;

use super::transition_resource;

#[repr(C)]
#[derive(Clone, Copy)]
struct ImGuiCBuffer {
    scale: [f32; 2],
    translation: [f32; 2],
}

pub struct ImGuiRenderer {
    root_signature: *mut ID3D12RootSignature,
    pipeline: *mut ID3D12PipelineState,

    vb_capacity: u64,
    ib_capacity: u64,
    vb_allocation: Allocation,
    ib_allocation: Allocation,
    vb_pointer: *mut u8,
    ib_pointer: *mut u8,
    vertex_buffer: *mut ID3D12Resource,
    index_buffer: *mut ID3D12Resource,

    cb_allocation: Allocation,
    cb_pointer: *mut u8,
    constant_buffer: *mut ID3D12Resource,

    font_image: *mut ID3D12Resource,
    font_image_memory: Allocation,
    font_image_srv_index: usize,

    font_image_upload_buffer: *mut ID3D12Resource,
    font_image_upload_buffer_memory: Allocation,
}

impl ImGuiRenderer {
    pub(crate) fn new(
        imgui: &mut imgui::Context,
        device: &mut ID3D12Device,
        allocator: &mut Allocator,
        descriptor_heap: &mut ID3D12DescriptorHeap,
        descriptor_heap_counter: &mut usize,
        cmd: &mut ID3D12GraphicsCommandList,
    ) -> Self {
        let root_signature = unsafe {
            let mut root_parameters = [
                D3D12_ROOT_PARAMETER {
                    ParameterType: D3D12_ROOT_PARAMETER_TYPE_CBV,
                    ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
                    ..Default::default()
                },
                D3D12_ROOT_PARAMETER {
                    ParameterType: D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
                    ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
                    ..Default::default()
                },
            ];

            let ranges = [D3D12_DESCRIPTOR_RANGE {
                RangeType: D3D12_DESCRIPTOR_RANGE_TYPE_SRV,
                NumDescriptors: 1,
                BaseShaderRegister: 2,
                RegisterSpace: 0,
                OffsetInDescriptorsFromTableStart: 0,
            }];

            root_parameters[0].u.Descriptor_mut().ShaderRegister = 0;
            root_parameters[0].u.Descriptor_mut().RegisterSpace = 0;
            root_parameters[1]
                .u
                .DescriptorTable_mut()
                .NumDescriptorRanges = ranges.len() as u32;
            root_parameters[1].u.DescriptorTable_mut().pDescriptorRanges = ranges.as_ptr();

            let static_samplers = [D3D12_STATIC_SAMPLER_DESC {
                Filter: D3D12_FILTER_MIN_MAG_MIP_LINEAR,
                AddressU: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
                AddressV: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
                AddressW: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
                MipLODBias: 0.0,
                MaxAnisotropy: 0,
                ComparisonFunc: D3D12_COMPARISON_FUNC_ALWAYS,
                BorderColor: D3D12_STATIC_BORDER_COLOR_OPAQUE_BLACK,
                MinLOD: 0.0,
                MaxLOD: 0.0,
                ShaderRegister: 1,
                RegisterSpace: 0,
                ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
            }];

            let rsig_desc = D3D12_ROOT_SIGNATURE_DESC {
                NumParameters: root_parameters.len() as u32,
                pParameters: root_parameters.as_ptr(),
                NumStaticSamplers: static_samplers.len() as u32,
                pStaticSamplers: static_samplers.as_ptr(),
                Flags: D3D12_ROOT_SIGNATURE_FLAG_ALLOW_INPUT_ASSEMBLER_INPUT_LAYOUT,
            };

            let mut blob = std::ptr::null_mut() as *mut ID3DBlob;
            let mut error_blob = std::ptr::null_mut() as *mut ID3DBlob;
            let hr = D3D12SerializeRootSignature(
                &rsig_desc,
                D3D_ROOT_SIGNATURE_VERSION_1,
                &mut blob as *mut _ as *mut _,
                &mut error_blob as *mut _ as *mut _,
            );
            if FAILED(hr) {
                panic!("Failed to serialize root signature. hr: {:#x}", hr); //TODO(max): Output error blob
            }

            let blob = blob.as_ref().unwrap();
            let mut rsig = std::ptr::null_mut() as *mut ID3D12RootSignature;
            let hr = device.CreateRootSignature(
                0,
                blob.GetBufferPointer(),
                blob.GetBufferSize(),
                &ID3D12RootSignature::uuidof(),
                &mut rsig as *mut _ as *mut _,
            );
            if FAILED(hr) {
                panic!("Failed to create root signature. hr: {:#x}", hr);
            }

            rsig.as_mut().unwrap()
        };

        let pipeline = unsafe {
            let vs = include_bytes!("./dxil/imgui.vs.dxil");
            let ps = include_bytes!("./dxil/imgui.ps.dxil");

            let input_elements = [
                D3D12_INPUT_ELEMENT_DESC {
                    SemanticName: b"POSITION\0".as_ptr().cast(),
                    SemanticIndex: 0,
                    Format: all_dxgi::DXGI_FORMAT_R32G32_FLOAT,
                    InputSlot: 0,
                    AlignedByteOffset: 0,
                    InputSlotClass: D3D12_INPUT_CLASSIFICATION_PER_VERTEX_DATA,
                    InstanceDataStepRate: 0,
                },
                D3D12_INPUT_ELEMENT_DESC {
                    SemanticName: b"TEXCOORD\0".as_ptr().cast(),
                    SemanticIndex: 0,
                    Format: all_dxgi::DXGI_FORMAT_R32G32_FLOAT,
                    InputSlot: 0,
                    AlignedByteOffset: 8,
                    InputSlotClass: D3D12_INPUT_CLASSIFICATION_PER_VERTEX_DATA,
                    InstanceDataStepRate: 0,
                },
                D3D12_INPUT_ELEMENT_DESC {
                    SemanticName: b"COLOR\0".as_ptr().cast(),
                    SemanticIndex: 0,
                    Format: all_dxgi::DXGI_FORMAT_R8G8B8A8_UNORM,
                    InputSlot: 0,
                    AlignedByteOffset: 16,
                    InputSlotClass: D3D12_INPUT_CLASSIFICATION_PER_VERTEX_DATA,
                    InstanceDataStepRate: 0,
                },
            ];

            let desc = D3D12_GRAPHICS_PIPELINE_STATE_DESC {
                pRootSignature: root_signature,
                VS: D3D12_SHADER_BYTECODE {
                    pShaderBytecode: vs.as_ptr().cast(),
                    BytecodeLength: vs.len(),
                },
                PS: D3D12_SHADER_BYTECODE {
                    pShaderBytecode: ps.as_ptr().cast(),
                    BytecodeLength: ps.len(),
                },
                BlendState: D3D12_BLEND_DESC {
                    AlphaToCoverageEnable: 0,
                    IndependentBlendEnable: 0,
                    RenderTarget: [
                        D3D12_RENDER_TARGET_BLEND_DESC {
                            BlendEnable: 1,
                            LogicOpEnable: 0,
                            SrcBlend: D3D12_BLEND_SRC_ALPHA,
                            DestBlend: D3D12_BLEND_INV_SRC_ALPHA,
                            BlendOp: D3D12_BLEND_OP_ADD,
                            SrcBlendAlpha: D3D12_BLEND_ONE,
                            DestBlendAlpha: D3D12_BLEND_ZERO,
                            BlendOpAlpha: D3D12_BLEND_OP_ADD,
                            LogicOp: D3D12_LOGIC_OP_NOOP,
                            RenderTargetWriteMask: D3D12_COLOR_WRITE_ENABLE_ALL as u8,
                        },
                        D3D12_RENDER_TARGET_BLEND_DESC::default(),
                        D3D12_RENDER_TARGET_BLEND_DESC::default(),
                        D3D12_RENDER_TARGET_BLEND_DESC::default(),
                        D3D12_RENDER_TARGET_BLEND_DESC::default(),
                        D3D12_RENDER_TARGET_BLEND_DESC::default(),
                        D3D12_RENDER_TARGET_BLEND_DESC::default(),
                        D3D12_RENDER_TARGET_BLEND_DESC::default(),
                    ],
                },
                SampleMask: !0u32,
                RasterizerState: D3D12_RASTERIZER_DESC {
                    FillMode: D3D12_FILL_MODE_SOLID,
                    CullMode: D3D12_CULL_MODE_NONE,
                    FrontCounterClockwise: 0,
                    DepthBias: 0,
                    DepthBiasClamp: 0.0,
                    SlopeScaledDepthBias: 0.0,
                    DepthClipEnable: 0,
                    MultisampleEnable: 0,
                    AntialiasedLineEnable: 0,
                    ForcedSampleCount: 1,
                    ConservativeRaster: D3D12_CONSERVATIVE_RASTERIZATION_MODE_OFF,
                },
                DepthStencilState: D3D12_DEPTH_STENCIL_DESC {
                    DepthEnable: 0,
                    DepthWriteMask: D3D12_DEPTH_WRITE_MASK_ZERO,
                    DepthFunc: D3D12_COMPARISON_FUNC_ALWAYS,
                    StencilEnable: 0,
                    StencilReadMask: 0,
                    StencilWriteMask: 0,
                    FrontFace: D3D12_DEPTH_STENCILOP_DESC::default(),
                    BackFace: D3D12_DEPTH_STENCILOP_DESC::default(),
                },
                InputLayout: D3D12_INPUT_LAYOUT_DESC {
                    pInputElementDescs: input_elements.as_ptr(),
                    NumElements: input_elements.len() as u32,
                },
                PrimitiveTopologyType: D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
                NumRenderTargets: 1,
                RTVFormats: [
                    all_dxgi::DXGI_FORMAT_R8G8B8A8_UNORM,
                    all_dxgi::DXGI_FORMAT_UNKNOWN,
                    all_dxgi::DXGI_FORMAT_UNKNOWN,
                    all_dxgi::DXGI_FORMAT_UNKNOWN,
                    all_dxgi::DXGI_FORMAT_UNKNOWN,
                    all_dxgi::DXGI_FORMAT_UNKNOWN,
                    all_dxgi::DXGI_FORMAT_UNKNOWN,
                    all_dxgi::DXGI_FORMAT_UNKNOWN,
                ],
                SampleDesc: all_dxgi::DXGI_SAMPLE_DESC {
                    Quality: 0,
                    Count: 1,
                },

                ..Default::default()
            };

            let mut pipeline: *mut ID3D12PipelineState = std::ptr::null_mut();
            let hr = device.CreateGraphicsPipelineState(
                &desc as *const _,
                &ID3D12PipelineState::uuidof(),
                &mut pipeline as *mut _ as *mut _,
            );
            if FAILED(hr) {
                panic!("Failed to create imgui pipeline.");
            }

            pipeline.as_mut().unwrap()
        };

        let (
            font_image,
            font_image_memory,
            font_image_srv_index,
            font_image_upload_buffer,
            font_image_upload_buffer_memory,
        ) = {
            let mut fonts = imgui.fonts();
            let font_atlas = fonts.build_rgba32_texture();

            let desc = D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
                Alignment: 0,
                Width: font_atlas.width as u64,
                Height: font_atlas.height,
                DepthOrArraySize: 1,
                MipLevels: 1,
                Format: all_dxgi::DXGI_FORMAT_R8G8B8A8_UNORM,
                SampleDesc: all_dxgi::DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
                Flags: D3D12_RESOURCE_FLAG_NONE,
            };
            let font_image_memory = allocator
                .allocate(&AllocationCreateDesc::from_d3d12_resource_desc(
                    device,
                    &desc,
                    "font_image",
                    MemoryLocation::GpuOnly,
                ))
                .unwrap();

            let font_image = unsafe {
                let mut font_image: *mut ID3D12Resource = std::ptr::null_mut();
                let hr = device.CreatePlacedResource(
                    font_image_memory.heap(),
                    font_image_memory.offset(),
                    &desc,
                    D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
                    std::ptr::null(),
                    &ID3D12Resource::uuidof(),
                    &mut font_image as *mut _ as *mut _,
                );
                if FAILED(hr) {
                    panic!("Failed to create font image. hr: {:#x}", hr);
                }

                font_image
            };

            //Create SRV
            let srv_index = unsafe {
                let srv_index = *descriptor_heap_counter;
                *descriptor_heap_counter += 1;

                let srv_size = device
                    .GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV)
                    as usize;

                let desc_heap_handle = descriptor_heap.GetCPUDescriptorHandleForHeapStart();
                let desc_heap_handle = D3D12_CPU_DESCRIPTOR_HANDLE {
                    ptr: desc_heap_handle.ptr + srv_index * srv_size,
                };

                let mut srv_desc = D3D12_SHADER_RESOURCE_VIEW_DESC {
                    Format: all_dxgi::DXGI_FORMAT_R8G8B8A8_UNORM,
                    ViewDimension: D3D12_SRV_DIMENSION_TEXTURE2D,
                    Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING(),
                    ..Default::default()
                };
                srv_desc.u.Texture2D_mut().MostDetailedMip = 0;
                srv_desc.u.Texture2D_mut().MipLevels = 1;
                srv_desc.u.Texture2D_mut().PlaneSlice = 0;
                srv_desc.u.Texture2D_mut().ResourceMinLODClamp = 0.0;

                device.CreateShaderResourceView(font_image, &srv_desc, desc_heap_handle);

                srv_index
            };

            let mut layouts = [D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default()];
            let mut num_rows: u32 = 0;
            let mut row_size_in_bytes: u64 = 0;
            let mut total_bytes: u64 = 0;
            unsafe {
                device.GetCopyableFootprints(
                    &font_image.as_ref().unwrap().GetDesc(),
                    0,                    // first sub
                    layouts.len() as u32, // num sub
                    0,                    // intermediate offset
                    layouts.as_mut_ptr(),
                    &mut num_rows as *mut _,
                    &mut row_size_in_bytes as *mut _,
                    &mut total_bytes as *mut _,
                )
            };

            // Create upload buffer
            let (upload_buffer, upload_buffer_memory) = {
                let desc = D3D12_RESOURCE_DESC {
                    Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                    Alignment: 0,
                    Width: total_bytes,
                    Height: 1,
                    DepthOrArraySize: 1,
                    MipLevels: 1,
                    Format: all_dxgi::DXGI_FORMAT_UNKNOWN,
                    SampleDesc: all_dxgi::DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                    Flags: D3D12_RESOURCE_FLAG_NONE,
                };

                let upload_buffer_memory = allocator
                    .allocate(&AllocationCreateDesc::from_d3d12_resource_desc(
                        device,
                        &desc,
                        "font_image upload buffer",
                        MemoryLocation::CpuToGpu,
                    ))
                    .unwrap();

                let mut upload_buffer: *mut ID3D12Resource = std::ptr::null_mut();
                let hr = unsafe {
                    device.CreatePlacedResource(
                        upload_buffer_memory.heap(),
                        upload_buffer_memory.offset(),
                        &desc,
                        D3D12_RESOURCE_STATE_GENERIC_READ,
                        std::ptr::null(),
                        &ID3D12Resource::uuidof(),
                        &mut upload_buffer as *mut _ as *mut _,
                    )
                };

                if FAILED(hr) {
                    panic!("Failed to create font image upload buffer. hr: {:x}.", hr);
                }

                (upload_buffer, upload_buffer_memory)
            };

            // TODO(max): Will this work correctly with strides and stuff?
            unsafe {
                let mut mapped_ptr = std::ptr::null_mut();
                upload_buffer
                    .as_ref()
                    .unwrap()
                    .Map(0, std::ptr::null(), &mut mapped_ptr as *mut _);
                std::ptr::copy_nonoverlapping(
                    font_atlas.data.as_ptr(),
                    mapped_ptr as *mut u8,
                    font_atlas.data.len(),
                );
                upload_buffer.as_ref().unwrap().Unmap(0, std::ptr::null())
            };

            let mut dst = D3D12_TEXTURE_COPY_LOCATION {
                pResource: font_image,
                Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                ..Default::default()
            };
            unsafe { *dst.u.SubresourceIndex_mut() = 0 };

            let mut src = D3D12_TEXTURE_COPY_LOCATION {
                pResource: upload_buffer,
                Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                ..Default::default()
            };
            unsafe { *src.u.PlacedFootprint_mut() = layouts[0] };

            unsafe {
                let barriers = [transition_resource(
                    font_image,
                    D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
                    D3D12_RESOURCE_STATE_COPY_DEST,
                )];
                cmd.ResourceBarrier(barriers.len() as u32, barriers.as_ptr());
                cmd.CopyTextureRegion(&dst, 0, 0, 0, &src, std::ptr::null())
            };

            (
                font_image,
                font_image_memory,
                srv_index,
                upload_buffer,
                upload_buffer_memory,
            )
        };

        let (constant_buffer, cb_allocation, cb_pointer) = {
            let desc = D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                Alignment: 0,
                Width: std::mem::size_of::<ImGuiCBuffer>() as u64,
                Height: 1,
                DepthOrArraySize: 1,
                MipLevels: 1,
                Format: all_dxgi::DXGI_FORMAT_UNKNOWN,
                SampleDesc: all_dxgi::DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                Flags: D3D12_RESOURCE_FLAG_NONE,
            };

            let allocation = allocator
                .allocate(&AllocationCreateDesc::from_d3d12_resource_desc(
                    device,
                    &desc,
                    "ImGui Constant buffer",
                    MemoryLocation::CpuToGpu,
                ))
                .unwrap();

            let mut buffer: *mut ID3D12Resource = std::ptr::null_mut();
            let hr = unsafe {
                device.CreatePlacedResource(
                    allocation.heap(),
                    allocation.offset(),
                    &desc,
                    D3D12_RESOURCE_STATE_VERTEX_AND_CONSTANT_BUFFER,
                    std::ptr::null(),
                    &ID3D12Resource::uuidof(),
                    &mut buffer as *mut _ as *mut _,
                )
            };
            if FAILED(hr) {
                panic!("Failed to create constant buffer. hr: {:x}.", hr);
            }

            let mut mapped_ptr: *mut u8 = std::ptr::null_mut();
            unsafe {
                buffer.as_ref().unwrap().Map(
                    0,
                    std::ptr::null(),
                    &mut mapped_ptr as *mut _ as *mut _,
                )
            };

            (buffer, allocation, mapped_ptr)
        };

        let vb_capacity = 1024 * 1024;
        let (vertex_buffer, vb_allocation, vb_pointer) = {
            let desc = D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                Alignment: 0,
                Width: vb_capacity,
                Height: 1,
                DepthOrArraySize: 1,
                MipLevels: 1,
                Format: all_dxgi::DXGI_FORMAT_UNKNOWN,
                SampleDesc: all_dxgi::DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                Flags: D3D12_RESOURCE_FLAG_NONE,
            };

            let allocation = allocator
                .allocate(&AllocationCreateDesc::from_d3d12_resource_desc(
                    device,
                    &desc,
                    "ImGui Vertex buffer",
                    MemoryLocation::CpuToGpu,
                ))
                .unwrap();

            let mut buffer: *mut ID3D12Resource = std::ptr::null_mut();
            let hr = unsafe {
                device.CreatePlacedResource(
                    allocation.heap(),
                    allocation.offset(),
                    &desc,
                    D3D12_RESOURCE_STATE_VERTEX_AND_CONSTANT_BUFFER,
                    std::ptr::null(),
                    &ID3D12Resource::uuidof(),
                    &mut buffer as *mut _ as *mut _,
                )
            };
            if FAILED(hr) {
                panic!("Failed to create vertex buffer. hr: {:x}.", hr);
            }

            let mut mapped_ptr: *mut u8 = std::ptr::null_mut();
            unsafe {
                buffer.as_ref().unwrap().Map(
                    0,
                    std::ptr::null(),
                    &mut mapped_ptr as *mut _ as *mut _,
                )
            };
            (buffer, allocation, mapped_ptr)
        };

        let ib_capacity = 1024 * 1024;
        let (index_buffer, ib_allocation, ib_pointer) = {
            let desc = D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                Alignment: 0,
                Width: ib_capacity,
                Height: 1,
                DepthOrArraySize: 1,
                MipLevels: 1,
                Format: all_dxgi::DXGI_FORMAT_UNKNOWN,
                SampleDesc: all_dxgi::DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                Flags: D3D12_RESOURCE_FLAG_NONE,
            };

            let allocation = allocator
                .allocate(&AllocationCreateDesc::from_d3d12_resource_desc(
                    device,
                    &desc,
                    "ImGui Vertex buffer",
                    MemoryLocation::CpuToGpu,
                ))
                .unwrap();

            let mut buffer: *mut ID3D12Resource = std::ptr::null_mut();
            let hr = unsafe {
                device.CreatePlacedResource(
                    allocation.heap(),
                    allocation.offset(),
                    &desc,
                    D3D12_RESOURCE_STATE_INDEX_BUFFER,
                    std::ptr::null(),
                    &ID3D12Resource::uuidof(),
                    &mut buffer as *mut _ as *mut _,
                )
            };
            if FAILED(hr) {
                panic!("Failed to create vertex buffer. hr: {:x}.", hr);
            }

            let mut mapped_ptr: *mut u8 = std::ptr::null_mut();
            unsafe {
                buffer.as_ref().unwrap().Map(
                    0,
                    std::ptr::null(),
                    &mut mapped_ptr as *mut _ as *mut _,
                )
            };
            (buffer, allocation, mapped_ptr)
        };

        Self {
            root_signature,
            pipeline,

            font_image,
            font_image_memory,
            font_image_srv_index,
            font_image_upload_buffer,
            font_image_upload_buffer_memory,

            cb_allocation,
            cb_pointer,
            constant_buffer,

            vb_capacity,
            ib_capacity,

            vb_pointer,
            ib_pointer,

            vb_allocation,
            ib_allocation,

            vertex_buffer,
            index_buffer,
        }
    }

    pub(crate) fn render(
        &mut self,
        imgui_draw_data: &imgui::DrawData,
        device: &mut ID3D12Device,
        window_width: u32,
        window_height: u32,
        descriptor_heap: &mut ID3D12DescriptorHeap,
        cmd: &mut ID3D12GraphicsCommandList,
    ) {
        // Update constant buffer
        {
            let left = imgui_draw_data.display_pos[0];
            let right = imgui_draw_data.display_pos[0] + imgui_draw_data.display_size[0];
            let top = imgui_draw_data.display_pos[1] + imgui_draw_data.display_size[1];
            let bottom = imgui_draw_data.display_pos[1];

            let cbuffer_data = ImGuiCBuffer {
                scale: [(2.0 / (right - left)), (2.0 / (bottom - top))],
                translation: [
                    (right + left) / (left - right),
                    (top + bottom) / (top - bottom),
                ],
            };

            unsafe { std::ptr::copy_nonoverlapping(&cbuffer_data, self.cb_pointer.cast(), 1) };
        }

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

        unsafe {
            let viewports = [D3D12_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: window_width as f32,
                Height: window_height as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            }];
            cmd.RSSetViewports(viewports.len() as u32, viewports.as_ptr());

            cmd.SetPipelineState(self.pipeline);
            cmd.SetGraphicsRootSignature(self.root_signature);
            cmd.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);

            {
                let constant_buffer = self.constant_buffer.as_mut().unwrap();
                let addr = constant_buffer.GetGPUVirtualAddress();
                cmd.SetGraphicsRootConstantBufferView(0, addr);
            }

            {
                let srv_stride =
                    device.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV);
                let heap_handle = descriptor_heap.GetGPUDescriptorHandleForHeapStart();
                let heap_handle = D3D12_GPU_DESCRIPTOR_HANDLE {
                    ptr: heap_handle.ptr + srv_stride as u64 * self.font_image_srv_index as u64,
                };
                cmd.SetGraphicsRootDescriptorTable(1, heap_handle);
            }
        }

        for draw_list in imgui_draw_data.draw_lists() {
            let vertices = draw_list.vtx_buffer();
            let indices = draw_list.idx_buffer();

            let vbv = {
                let vertex_buffer = unsafe { self.vertex_buffer.as_mut().unwrap() };
                let stride = std::mem::size_of::<imgui::DrawVert>();
                let address =
                    unsafe { vertex_buffer.GetGPUVirtualAddress() } + (vb_offset * stride) as u64;

                D3D12_VERTEX_BUFFER_VIEW {
                    BufferLocation: address,
                    SizeInBytes: (vertices.len() * stride) as u32,
                    StrideInBytes: stride as u32,
                }
            };
            let ibv = {
                let index_buffer = unsafe { self.index_buffer.as_mut().unwrap() };
                let stride = std::mem::size_of::<u16>();
                let address =
                    unsafe { index_buffer.GetGPUVirtualAddress() } + (ib_offset * stride) as u64;

                D3D12_INDEX_BUFFER_VIEW {
                    BufferLocation: address,
                    SizeInBytes: (indices.len() * stride) as u32,
                    Format: all_dxgi::DXGI_FORMAT_R16_UINT,
                }
            };

            // Upload vertices
            unsafe {
                let stride = std::mem::size_of::<imgui::DrawVert>();
                let dst_ptr = self
                    .vb_pointer
                    .add(vb_offset * stride)
                    .cast::<imgui::DrawVert>();
                std::ptr::copy_nonoverlapping(vertices.as_ptr(), dst_ptr, vertices.len());
            }
            vb_offset += vertices.len();

            // Upload indices
            unsafe {
                let stride = std::mem::size_of::<u16>();
                let dst_ptr = self.ib_pointer.add(ib_offset * stride).cast();
                std::ptr::copy_nonoverlapping(indices.as_ptr(), dst_ptr, indices.len());
            }
            ib_offset += indices.len();

            unsafe {
                cmd.IASetVertexBuffers(0, 1, &vbv as *const _);
                cmd.IASetIndexBuffer(&ibv as *const _);
            };
            for command in draw_list.commands() {
                match command {
                    imgui::DrawCmd::Elements { count, cmd_params } => {
                        let scissor_rect = D3D12_RECT {
                            left: cmd_params.clip_rect[0] as i32,
                            top: cmd_params.clip_rect[1] as i32,
                            right: cmd_params.clip_rect[2] as i32,
                            bottom: cmd_params.clip_rect[3] as i32,
                        };
                        unsafe {
                            cmd.RSSetScissorRects(1, &scissor_rect as *const _);
                            cmd.DrawIndexedInstanced(
                                count as u32,
                                1,
                                cmd_params.idx_offset as u32,
                                cmd_params.vtx_offset as i32,
                                0,
                            );
                        };
                    }
                    _ => todo!(),
                }
            }
        }
    }

    pub(crate) fn destroy(self, allocator: &mut Allocator) {
        unsafe { self.pipeline.as_ref().unwrap().Release() };

        unsafe { self.font_image_upload_buffer.as_ref().unwrap().Release() };
        unsafe { self.vertex_buffer.as_ref().unwrap().Release() };
        unsafe { self.index_buffer.as_ref().unwrap().Release() };
        unsafe { self.constant_buffer.as_ref().unwrap().Release() };
        unsafe { self.font_image.as_ref().unwrap().Release() };

        allocator
            .free(self.font_image_upload_buffer_memory)
            .unwrap();
        allocator.free(self.vb_allocation).unwrap();
        allocator.free(self.ib_allocation).unwrap();
        allocator.free(self.cb_allocation).unwrap();
        allocator.free(self.font_image_memory).unwrap();
    }
}

pub(crate) fn handle_imgui_event(
    io: &mut imgui::Io,
    window: &winit::window::Window,
    event: &winit::event::Event<()>,
) -> bool {
    use winit::event::{
        DeviceEvent, ElementState, Event, KeyboardInput, MouseButton, MouseScrollDelta, TouchPhase,
        VirtualKeyCode, WindowEvent,
    };

    match event {
        Event::WindowEvent { event, window_id } if *window_id == window.id() => match *event {
            WindowEvent::Resized(physical_size) => {
                io.display_size = [physical_size.width as f32, physical_size.height as f32];
                false
            }
            WindowEvent::KeyboardInput {
                input:
                    KeyboardInput {
                        virtual_keycode: Some(key),
                        state,
                        ..
                    },
                ..
            } => {
                let pressed = state == ElementState::Pressed;
                io.keys_down[key as usize] = pressed;
                match key {
                    VirtualKeyCode::LShift | VirtualKeyCode::RShift => io.key_shift = pressed,
                    VirtualKeyCode::LControl | VirtualKeyCode::RControl => io.key_ctrl = pressed,
                    VirtualKeyCode::LAlt | VirtualKeyCode::RAlt => io.key_alt = pressed,
                    VirtualKeyCode::LWin | VirtualKeyCode::RWin => io.key_super = pressed,
                    _ => (),
                }

                io.want_capture_keyboard
            }
            WindowEvent::ReceivedCharacter(ch) => {
                io.add_input_character(ch);

                io.want_capture_keyboard
            }

            WindowEvent::CursorMoved { position, .. } => {
                io.mouse_pos = [position.x as f32, position.y as f32];

                io.want_capture_mouse
            }
            WindowEvent::MouseWheel {
                delta,
                phase: TouchPhase::Moved,
                ..
            } => {
                match delta {
                    MouseScrollDelta::LineDelta(h, v) => {
                        io.mouse_wheel_h = h;
                        io.mouse_wheel = v;
                    }
                    MouseScrollDelta::PixelDelta(pos) => {
                        match pos.x.partial_cmp(&0.0) {
                            Some(std::cmp::Ordering::Greater) => io.mouse_wheel_h += 1.0,
                            Some(std::cmp::Ordering::Less) => io.mouse_wheel_h -= 1.0,
                            _ => (),
                        }
                        match pos.y.partial_cmp(&0.0) {
                            Some(std::cmp::Ordering::Greater) => io.mouse_wheel += 1.0,
                            Some(std::cmp::Ordering::Less) => io.mouse_wheel -= 1.0,
                            _ => (),
                        }
                    }
                }

                io.want_capture_mouse
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let pressed = state == ElementState::Pressed;
                match button {
                    MouseButton::Left => io.mouse_down[0] = pressed,
                    MouseButton::Right => io.mouse_down[1] = pressed,
                    MouseButton::Middle => io.mouse_down[2] = pressed,
                    MouseButton::Other(idx @ 0..=4) => io.mouse_down[idx as usize] = pressed,
                    _ => (),
                }

                io.want_capture_mouse
            }
            _ => false,
        },
        // Track key release events outside our window. If we don't do this,
        // we might never see the release event if some other window gets focus.
        Event::DeviceEvent {
            event:
                DeviceEvent::Key(KeyboardInput {
                    state: ElementState::Released,
                    virtual_keycode: Some(key),
                    ..
                }),
            ..
        } => {
            io.keys_down[*key as usize] = false;
            match *key {
                VirtualKeyCode::LShift | VirtualKeyCode::RShift => io.key_shift = false,
                VirtualKeyCode::LControl | VirtualKeyCode::RControl => io.key_ctrl = false,
                VirtualKeyCode::LAlt | VirtualKeyCode::RAlt => io.key_alt = false,
                VirtualKeyCode::LWin | VirtualKeyCode::RWin => io.key_super = false,
                _ => (),
            }

            io.want_capture_keyboard
        }
        _ => false,
    }
}
