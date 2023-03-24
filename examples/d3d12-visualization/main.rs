#![windows_subsystem = "windows"]
//! Example showcasing [`winapi`] interop with [`gpu-allocator`] which is driven by the [`windows`] crate.
use gpu_allocator::AllocationSizes;
use log::info;
use raw_window_handle::HasRawWindowHandle;

use gpu_allocator::d3d12::{Allocator, AllocatorCreateDesc, ToWindows};

mod all_dxgi {
    pub use winapi::shared::{
        dxgi::*, dxgi1_2::*, dxgi1_3::*, dxgi1_4::*, dxgi1_6::*, dxgiformat::*, dxgitype::*,
    };
}

use winapi::um::d3d12::*;
use winapi::um::d3dcommon::*;
use winapi::um::winuser;

use winapi::shared::minwindef::UINT;
use winapi::shared::winerror;
use winapi::shared::winerror::{FAILED, SUCCEEDED};

use winapi::Interface;

mod imgui_renderer;
use imgui_renderer::{handle_imgui_event, ImGuiRenderer};

const ENABLE_DEBUG_LAYER: bool = true;
const FRAMES_IN_FLIGHT: usize = 2;

struct BackBuffer {
    resource: *mut ID3D12Resource,
    rtv_handle: D3D12_CPU_DESCRIPTOR_HANDLE,
}

fn find_hardware_adapter(
    dxgi_factory: &all_dxgi::IDXGIFactory6,
) -> Option<*mut all_dxgi::IDXGIAdapter4> {
    let mut adapter: *mut all_dxgi::IDXGIAdapter4 = std::ptr::null_mut();
    for adapter_index in 0.. {
        let hr = unsafe {
            dxgi_factory.EnumAdapters1(
                adapter_index,
                <*mut *mut all_dxgi::IDXGIAdapter4>::cast(&mut adapter),
            )
        };
        if hr == winerror::DXGI_ERROR_NOT_FOUND {
            break;
        }

        let mut desc = Default::default();
        unsafe { adapter.as_ref().unwrap().GetDesc3(&mut desc) };

        if (desc.Flags & all_dxgi::DXGI_ADAPTER_FLAG_SOFTWARE) != 0 {
            continue;
        }

        let hr = unsafe {
            D3D12CreateDevice(
                adapter.cast(),
                D3D_FEATURE_LEVEL_12_0,
                &IID_ID3D12Device,
                std::ptr::null_mut(),
            )
        };
        if SUCCEEDED(hr) {
            return Some(adapter);
        }
    }

    None
}

fn enable_d3d12_debug_layer() -> bool {
    use winapi::um::d3d12sdklayers::ID3D12Debug;
    let mut debug: *mut ID3D12Debug = std::ptr::null_mut();
    let hr = unsafe {
        D3D12GetDebugInterface(
            &ID3D12Debug::uuidof(),
            <*mut *mut ID3D12Debug>::cast(&mut debug),
        )
    };
    if FAILED(hr) {
        return false;
    }

    let debug = unsafe { debug.as_mut().unwrap() };
    unsafe { debug.EnableDebugLayer() };
    unsafe { debug.Release() };

    true
}

fn create_d3d12_device(adapter: &mut all_dxgi::IDXGIAdapter4) -> *mut ID3D12Device {
    unsafe {
        let mut device: *mut ID3D12Device = std::ptr::null_mut();
        let hr = D3D12CreateDevice(
            <*mut all_dxgi::IDXGIAdapter4>::cast(adapter),
            D3D_FEATURE_LEVEL_12_0,
            &ID3D12Device::uuidof(),
            <*mut *mut ID3D12Device>::cast(&mut device),
        );
        if FAILED(hr) {
            panic!("Failed to create ID3D12Device.");
        }
        device
    }
}

#[must_use]
fn transition_resource(
    resource: *mut ID3D12Resource,
    before: D3D12_RESOURCE_STATES,
    after: D3D12_RESOURCE_STATES,
) -> D3D12_RESOURCE_BARRIER {
    let mut barrier = D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        ..D3D12_RESOURCE_BARRIER::default()
    };

    unsafe {
        barrier.u.Transition_mut().pResource = resource;
        barrier.u.Transition_mut().StateBefore = before;
        barrier.u.Transition_mut().StateAfter = after;
        barrier.u.Transition_mut().Subresource = D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES;
    }

    barrier
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("trace")).init();

    // Disable automatic DPI scaling by windows
    unsafe { winuser::SetProcessDPIAware() };

    let event_loop = winit::event_loop::EventLoop::new();

    let window_width = 1920;
    let window_height = 1080;
    let window = winit::window::WindowBuilder::new()
        .with_title("gpu-allocator d3d12 visualization")
        .with_inner_size(winit::dpi::PhysicalSize::new(
            window_width as f64,
            window_height as f64,
        ))
        .with_resizable(false)
        .build(&event_loop)
        .unwrap();

    let (event_send, event_recv) = std::sync::mpsc::sync_channel(1);
    let quit_send = event_loop.create_proxy();

    std::thread::spawn(move || {
        let mut dxgi_factory_flags = 0;
        if ENABLE_DEBUG_LAYER && enable_d3d12_debug_layer() {
            info!("Enabled D3D12 debug layer");
            dxgi_factory_flags |= all_dxgi::DXGI_CREATE_FACTORY_DEBUG;
        }

        let dxgi_factory = unsafe {
            let mut factory: *mut all_dxgi::IDXGIFactory6 = std::ptr::null_mut();
            let hr = all_dxgi::CreateDXGIFactory2(
                dxgi_factory_flags,
                &all_dxgi::IID_IDXGIFactory6,
                <*mut *mut all_dxgi::IDXGIFactory6>::cast(&mut factory),
            );

            if FAILED(hr) {
                panic!("Failed to create dxgi factory");
            }

            factory.as_mut().unwrap()
        };

        let adapter = find_hardware_adapter(dxgi_factory).unwrap();
        let adapter = unsafe { adapter.as_mut().unwrap() };

        let device = create_d3d12_device(adapter);
        let device = unsafe { device.as_mut().unwrap() };

        let queue = unsafe {
            let desc = D3D12_COMMAND_QUEUE_DESC {
                Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
                Priority: 0, // ?
                Flags: D3D12_COMMAND_QUEUE_FLAG_NONE,
                NodeMask: 0,
            };

            let mut queue: *mut ID3D12CommandQueue = std::ptr::null_mut();
            let hr = device.CreateCommandQueue(
                &desc,
                &ID3D12CommandQueue::uuidof(),
                <*mut *mut ID3D12CommandQueue>::cast(&mut queue),
            );
            if FAILED(hr) {
                panic!("Failed to create command queue.");
            }

            queue.as_mut().unwrap()
        };

        let swapchain = unsafe {
            let mut swapchain: *mut all_dxgi::IDXGISwapChain3 = std::ptr::null_mut();

            let swap_chain_desc = all_dxgi::DXGI_SWAP_CHAIN_DESC1 {
                BufferCount: FRAMES_IN_FLIGHT as UINT,
                Width: window_width,
                Height: window_height,
                Format: all_dxgi::DXGI_FORMAT_R8G8B8A8_UNORM,
                BufferUsage: all_dxgi::DXGI_USAGE_RENDER_TARGET_OUTPUT,
                SwapEffect: all_dxgi::DXGI_SWAP_EFFECT_FLIP_DISCARD,
                SampleDesc: all_dxgi::DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                ..all_dxgi::DXGI_SWAP_CHAIN_DESC1::default()
            };

            let raw_window_haver: &dyn HasRawWindowHandle = &window;
            let hwnd = if let raw_window_handle::RawWindowHandle::Win32(handle) =
                raw_window_haver.raw_window_handle()
            {
                handle.hwnd
            } else {
                panic!("Failed to get HWND.")
            };
            let hr = dxgi_factory.CreateSwapChainForHwnd(
                <*mut ID3D12CommandQueue>::cast(queue),
                hwnd.cast(),
                &swap_chain_desc,
                std::ptr::null(),
                std::ptr::null_mut(),
                <*mut *mut all_dxgi::IDXGISwapChain3>::cast(&mut swapchain),
            );
            if FAILED(hr) {
                panic!("Failed to create swapchain. hr: {:#x}", hr);
            }
            swapchain.as_mut().unwrap()
        };

        let rtv_heap = unsafe {
            let desc = D3D12_DESCRIPTOR_HEAP_DESC {
                Type: D3D12_DESCRIPTOR_HEAP_TYPE_RTV,
                NumDescriptors: 2,
                Flags: D3D12_DESCRIPTOR_HEAP_FLAG_NONE,
                NodeMask: 0,
            };
            let mut heap: *mut ID3D12DescriptorHeap = std::ptr::null_mut();
            let hr = device.CreateDescriptorHeap(
                &desc,
                &IID_ID3D12DescriptorHeap,
                <*mut *mut ID3D12DescriptorHeap>::cast(&mut heap),
            );
            if FAILED(hr) {
                panic!("Failed to create RTV Descriptor heap");
            }

            heap.as_mut().unwrap()
        };

        let backbuffers = unsafe {
            (0..FRAMES_IN_FLIGHT)
                .map(|i| {
                    let mut resource: *mut ID3D12Resource = std::ptr::null_mut();
                    let hr = swapchain.GetBuffer(
                        i as u32,
                        &ID3D12Resource::uuidof(),
                        <*mut *mut ID3D12Resource>::cast(&mut resource),
                    );
                    if FAILED(hr) {
                        panic!("Failed to access swapchain buffer {}", i);
                    }

                    let mut u = D3D12_RENDER_TARGET_VIEW_DESC_u::default();
                    let t2d = u.Texture2D_mut();
                    t2d.MipSlice = 0;
                    t2d.PlaneSlice = 0;

                    let rtv_stride = device
                        .GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_RTV)
                        as usize;
                    let rtv_handle = D3D12_CPU_DESCRIPTOR_HANDLE {
                        ptr: rtv_heap.GetCPUDescriptorHandleForHeapStart().ptr + i * rtv_stride,
                    };

                    let mut rtv_desc = D3D12_RENDER_TARGET_VIEW_DESC {
                        Format: all_dxgi::DXGI_FORMAT_R8G8B8A8_UNORM,
                        ViewDimension: D3D12_RTV_DIMENSION_TEXTURE2D,
                        ..Default::default()
                    };
                    rtv_desc.u.Texture2D_mut().MipSlice = 0;
                    rtv_desc.u.Texture2D_mut().PlaneSlice = 0;

                    device.CreateRenderTargetView(resource, &rtv_desc, rtv_handle);

                    BackBuffer {
                        resource,
                        rtv_handle,
                    }
                })
                .collect::<Vec<_>>()
        };

        let command_allocator = unsafe {
            let mut command_allocator: *mut ID3D12CommandAllocator = std::ptr::null_mut();

            let hr = device.CreateCommandAllocator(
                D3D12_COMMAND_LIST_TYPE_DIRECT,
                &ID3D12CommandAllocator::uuidof(),
                <*mut *mut ID3D12CommandAllocator>::cast(&mut command_allocator),
            );
            if FAILED(hr) {
                panic!("Failed to create command allocator");
            }

            command_allocator.as_mut().unwrap()
        };

        let descriptor_heap = unsafe {
            let desc = D3D12_DESCRIPTOR_HEAP_DESC {
                Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
                NumDescriptors: 4096,
                Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                NodeMask: 0,
            };

            let mut heap: *mut ID3D12DescriptorHeap = std::ptr::null_mut();
            let hr = device.CreateDescriptorHeap(
                &desc,
                &IID_ID3D12DescriptorHeap,
                <*mut *mut ID3D12DescriptorHeap>::cast(&mut heap),
            );
            if FAILED(hr) {
                panic!("Failed to create descriptor heap.");
            }

            heap.as_mut().unwrap()
        };

        let command_list = unsafe {
            let mut command_list: *mut ID3D12GraphicsCommandList = std::ptr::null_mut();
            let hr = device.CreateCommandList(
                0,
                D3D12_COMMAND_LIST_TYPE_DIRECT,
                command_allocator,
                std::ptr::null_mut(),
                &ID3D12GraphicsCommandList::uuidof(),
                <*mut *mut ID3D12GraphicsCommandList>::cast(&mut command_list),
            );
            if FAILED(hr) {
                panic!("Failed to create command list.");
            }

            command_list.as_mut().unwrap()
        };

        let mut allocator = Allocator::new(&AllocatorCreateDesc {
            device: device.as_windows().clone(),
            debug_settings: Default::default(),
            allocation_sizes: Default::default(),
        })
        .unwrap();

        let mut descriptor_heap_counter = 0;

        let mut imgui = imgui::Context::create();
        imgui.io_mut().display_size = [window_width as f32, window_height as f32];
        let mut imgui_renderer = ImGuiRenderer::new(
            &mut imgui,
            device,
            &mut allocator,
            descriptor_heap,
            &mut descriptor_heap_counter,
            command_list,
        );

        let fence = unsafe {
            let mut fence: *mut ID3D12Fence = std::ptr::null_mut();
            let hr = device.CreateFence(
                0,
                D3D12_FENCE_FLAG_NONE,
                &ID3D12Fence::uuidof(),
                <*mut *mut ID3D12Fence>::cast(&mut fence),
            );
            if FAILED(hr) {
                panic!("Failed to create fence");
            }

            fence.as_mut().unwrap()
        };

        let mut fence_value = 0_u64;

        unsafe { command_list.Close() };

        // Submit and wait idle
        unsafe {
            let lists = [<*mut ID3D12GraphicsCommandList>::cast(command_list)];
            queue.ExecuteCommandLists(lists.len() as u32, lists.as_ptr());
            fence_value += 1;
            queue.Signal(fence, fence_value);

            while fence.GetCompletedValue() < fence_value {}
        };

        let mut visualizer = gpu_allocator::d3d12::AllocatorVisualizer::new();

        loop {
            let event = event_recv.recv().unwrap();
            handle_imgui_event(imgui.io_mut(), &window, &event);

            let mut should_quit = false;
            if let winit::event::Event::WindowEvent { event, .. } = event {
                match event {
                    winit::event::WindowEvent::KeyboardInput { input, .. } => {
                        if let Some(winit::event::VirtualKeyCode::Escape) = input.virtual_keycode {
                            should_quit = true;
                        }
                    }
                    winit::event::WindowEvent::CloseRequested => {
                        should_quit = true;
                    }
                    _ => {}
                }
            }

            if should_quit {
                quit_send.send_event(()).unwrap();
                break;
            }

            let buffer_index = unsafe { swapchain.GetCurrentBackBufferIndex() };
            let current_backbuffer = &backbuffers[buffer_index as usize];

            let ui = imgui.frame();
            visualizer.render(&allocator, ui, None);
            let imgui_draw_data = imgui.render();

            unsafe {
                command_allocator.Reset();
                command_list.Reset(command_allocator, std::ptr::null_mut());

                {
                    let barriers = [transition_resource(
                        current_backbuffer.resource,
                        D3D12_RESOURCE_STATE_PRESENT,
                        D3D12_RESOURCE_STATE_RENDER_TARGET,
                    )];
                    command_list.ResourceBarrier(barriers.len() as u32, barriers.as_ptr());
                }

                command_list.ClearRenderTargetView(
                    current_backbuffer.rtv_handle,
                    &[1.0, 1.0, 0.0, 0.0],
                    0,
                    std::ptr::null_mut(),
                );

                let rtv_handles = [current_backbuffer.rtv_handle];
                command_list.OMSetRenderTargets(
                    rtv_handles.len() as u32,
                    rtv_handles.as_ptr(),
                    0,
                    std::ptr::null_mut(),
                );

                {
                    let scissor_rects = [D3D12_RECT {
                        left: 0,
                        top: 0,
                        right: window_width as i32,
                        bottom: window_height as i32,
                    }];
                    command_list
                        .RSSetScissorRects(scissor_rects.len() as u32, scissor_rects.as_ptr());
                }

                let mut heaps: [*mut _; 1] = [descriptor_heap];
                command_list.SetDescriptorHeaps(heaps.len() as u32, heaps.as_mut_ptr());

                imgui_renderer.render(
                    imgui_draw_data,
                    device,
                    window_width,
                    window_height,
                    descriptor_heap,
                    command_list,
                );

                {
                    let barriers = [transition_resource(
                        current_backbuffer.resource,
                        D3D12_RESOURCE_STATE_RENDER_TARGET,
                        D3D12_RESOURCE_STATE_PRESENT,
                    )];
                    command_list.ResourceBarrier(barriers.len() as u32, barriers.as_ptr());
                }

                command_list.Close();

                let lists = [<*mut ID3D12GraphicsCommandList>::cast(command_list)];
                queue.ExecuteCommandLists(lists.len() as u32, lists.as_ptr());
            }

            unsafe { swapchain.Present(0, 0) };

            unsafe {
                fence_value += 1;
                queue.Signal(fence, fence_value);

                loop {
                    if fence_value == fence.GetCompletedValue() {
                        break;
                    }
                }
            }
        }

        unsafe {
            fence_value += 1;
            queue.Signal(fence, fence_value);

            while fence.GetCompletedValue() < fence_value {}
        }

        imgui_renderer.destroy(&mut allocator);

        unsafe {
            for b in backbuffers {
                b.resource.as_ref().unwrap().Release();
            }

            fence.Release();
            command_list.Release();
            command_allocator.Release();
            swapchain.Release();

            queue.Release();
            device.Release();
            adapter.Release();
            dxgi_factory.Release();
        }
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
