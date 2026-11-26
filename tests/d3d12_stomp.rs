//! Device-backed tests for D3D12 stomp mode. Runs on real hardware when present, else on
//! WARP, so it also runs on CI. WARP does not page-fault; the fault tests live in
//! `d3d12_stomp_fault.rs`.
#![cfg(all(windows, feature = "d3d12"))]

use std::mem::size_of;

use gpu_allocator::{
    d3d12::{
        AllocationCreateDesc, Allocator, AllocatorCreateDesc, ID3D12DeviceVersion, Resource,
        ResourceCategory, ResourceCreateDesc, ResourceStateOrBarrierLayout, ResourceType,
        StompMode, StompSettings,
    },
    AllocationError, MemoryLocation,
};
use windows::{
    core::Interface,
    Win32::Graphics::{
        Direct3D::D3D_FEATURE_LEVEL_11_0,
        Direct3D12::*,
        Dxgi::{
            Common::{
                DXGI_FORMAT, DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC,
            },
            CreateDXGIFactory2, IDXGIAdapter, IDXGIAdapter1, IDXGIFactory6,
            DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_CREATE_FACTORY_FLAGS, DXGI_ERROR_NOT_FOUND,
        },
    },
};

const TILE: u64 = D3D12_TILED_RESOURCE_TILE_SIZE_IN_BYTES as u64;

struct TestDevice {
    device: ID3D12Device,
}

/// Debug layer on, first hardware adapter, else WARP. Panics without any device: a silent
/// skip would hide breakage.
fn make_device() -> TestDevice {
    let mut debug: Option<ID3D12Debug> = None;
    if unsafe { D3D12GetDebugInterface(&mut debug) }.is_ok() {
        unsafe { debug.unwrap().EnableDebugLayer() };
    }

    let factory: IDXGIFactory6 =
        unsafe { CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)) }.expect("DXGI factory");

    for idx in 0.. {
        let adapter: IDXGIAdapter1 = match unsafe { factory.EnumAdapters1(idx) } {
            Ok(a) => a,
            Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(e) => panic!("EnumAdapters1: {e}"),
        };
        let desc = unsafe { adapter.GetDesc1() }.unwrap();
        if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
            continue;
        }
        let mut device: Option<ID3D12Device> = None;
        if unsafe { D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_11_0, &mut device) }.is_ok() {
            println!("adapter: {}", wide(&desc.Description));
            return TestDevice {
                device: device.unwrap(),
            };
        }
    }

    let warp: IDXGIAdapter = unsafe { factory.EnumWarpAdapter() }.expect("WARP adapter");
    let mut device: Option<ID3D12Device> = None;
    unsafe { D3D12CreateDevice(&warp, D3D_FEATURE_LEVEL_11_0, &mut device) }
        .expect("no hardware adapter and WARP device creation failed");
    println!("adapter: WARP");
    TestDevice {
        device: device.unwrap(),
    }
}

fn wide(s: &[u16]) -> String {
    let len = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..len])
}

/// Fail on any ERROR or CORRUPTION message the debug layer stored for this device.
/// Warnings are printed so mapping onto the never-resident heap can be inspected.
fn assert_no_d3d12_errors(device: &ID3D12Device) {
    let Ok(queue) = device.cast::<ID3D12InfoQueue>() else {
        println!("no ID3D12InfoQueue, debug layer unavailable");
        return;
    };
    let mut errors = Vec::new();
    for i in 0..unsafe { queue.GetNumStoredMessages() } {
        let mut len = 0usize;
        let _ = unsafe { queue.GetMessage(i, None, &mut len) };
        // u64 storage keeps the D3D12_MESSAGE header aligned.
        let mut buf = vec![0u64; (len.max(size_of::<D3D12_MESSAGE>()) + 7) / 8];
        let msg = buf.as_mut_ptr().cast::<D3D12_MESSAGE>();
        if unsafe { queue.GetMessage(i, Some(msg), &mut len) }.is_err() {
            continue;
        }
        let msg = unsafe { &*msg };
        let text = unsafe { std::ffi::CStr::from_ptr(msg.pDescription.cast()) }
            .to_string_lossy()
            .into_owned();
        if msg.Severity == D3D12_MESSAGE_SEVERITY_ERROR
            || msg.Severity == D3D12_MESSAGE_SEVERITY_CORRUPTION
        {
            errors.push(text);
        } else if msg.Severity == D3D12_MESSAGE_SEVERITY_WARNING {
            println!("d3d12 warning: {text}");
        }
    }
    assert!(
        errors.is_empty(),
        "D3D12 debug layer errors:\n{}",
        errors.join("\n")
    );
}

fn tiled_tier(device: &ID3D12Device) -> D3D12_TILED_RESOURCES_TIER {
    let mut options = D3D12_FEATURE_DATA_D3D12_OPTIONS::default();
    unsafe {
        device.CheckFeatureSupport(
            D3D12_FEATURE_D3D12_OPTIONS,
            <*mut D3D12_FEATURE_DATA_D3D12_OPTIONS>::cast(&mut options),
            size_of::<D3D12_FEATURE_DATA_D3D12_OPTIONS>() as u32,
        )
    }
    .unwrap();
    options.TiledResourcesTier
}

fn buffer_desc(width: u64) -> D3D12_RESOURCE_DESC {
    D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
        Alignment: 0,
        Width: width,
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
    }
}

fn tex_desc(
    dimension: D3D12_RESOURCE_DIMENSION,
    width: u64,
    height: u32,
    depth: u16,
    format: DXGI_FORMAT,
    flags: D3D12_RESOURCE_FLAGS,
    samples: u32,
) -> D3D12_RESOURCE_DESC {
    D3D12_RESOURCE_DESC {
        Dimension: dimension,
        Alignment: 0,
        Width: width,
        Height: height,
        DepthOrArraySize: depth,
        MipLevels: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: samples,
            Quality: 0,
        },
        Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
        Flags: flags,
    }
}

fn tex2d_desc(flags: D3D12_RESOURCE_FLAGS, samples: u32) -> D3D12_RESOURCE_DESC {
    tex_desc(
        D3D12_RESOURCE_DIMENSION_TEXTURE2D,
        256,
        256,
        1,
        DXGI_FORMAT_R8G8B8A8_UNORM,
        flags,
        samples,
    )
}

/// A dedicated copy queue for the tile mapping updates, as the docs recommend.
fn copy_queue(device: &ID3D12Device) -> ID3D12CommandQueue {
    unsafe {
        device.CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
            Type: D3D12_COMMAND_LIST_TYPE_COPY,
            ..Default::default()
        })
    }
    .unwrap()
}

fn settings(td: &TestDevice) -> StompSettings {
    StompSettings::new(copy_queue(&td.device))
}

fn try_allocator(
    td: &TestDevice,
    stomp: Option<StompSettings>,
) -> Result<Allocator, AllocationError> {
    Allocator::new(&AllocatorCreateDesc {
        device: ID3D12DeviceVersion::Device(td.device.clone()),
        debug_settings: Default::default(),
        allocation_sizes: Default::default(),
        stomp,
    })
}

fn stomp_allocator(td: &TestDevice, settings: StompSettings) -> Allocator {
    try_allocator(td, Some(settings)).unwrap()
}

fn plain_allocator(td: &TestDevice) -> Allocator {
    try_allocator(td, None).unwrap()
}

fn create<'a>(
    alloc: &mut Allocator,
    name: &str,
    desc: &D3D12_RESOURCE_DESC,
    location: MemoryLocation,
    stomp: Option<bool>,
    resource_type: &ResourceType<'a>,
    clear_value: Option<&D3D12_CLEAR_VALUE>,
) -> Resource {
    alloc
        .create_resource(&ResourceCreateDesc {
            name,
            memory_location: location,
            resource_category: desc.into(),
            resource_desc: desc,
            castable_formats: &[],
            clear_value,
            initial_state_or_layout: ResourceStateOrBarrierLayout::ResourceState(
                D3D12_RESOURCE_STATE_COMMON,
            ),
            resource_type,
            stomp,
        })
        .unwrap()
}

fn create_tex(alloc: &mut Allocator, name: &str, desc: &D3D12_RESOURCE_DESC) -> Resource {
    create(
        alloc,
        name,
        desc,
        MemoryLocation::GpuOnly,
        None,
        &ResourceType::Placed,
        None,
    )
}

fn create_buffer(
    alloc: &mut Allocator,
    name: &str,
    width: u64,
    location: MemoryLocation,
    stomp: Option<bool>,
) -> Resource {
    create(
        alloc,
        name,
        &buffer_desc(width),
        location,
        stomp,
        &ResourceType::Placed,
        None,
    )
}

fn va(r: &Resource) -> u64 {
    unsafe { r.resource().GetGPUVirtualAddress() }
}

// 1
#[test]
fn plain_allocator_unchanged() {
    let td = make_device();
    let mut alloc = plain_allocator(&td);
    assert!(alloc.stomp_statistics().is_none());

    let r = create_buffer(&mut alloc, "plain", 4000, MemoryLocation::GpuOnly, None);
    assert!(r.allocation.is_some());
    assert!(r.stomp_layout().is_none());
    assert!(!r.is_stomp_guarded());
    assert_eq!(r.offset(), 0);
    assert_eq!(unsafe { r.resource().GetDesc() }.Width, 4000);
    assert!(alloc.capacity() > 0);
    assert_eq!(alloc.generate_report().allocations.len(), 1);
    alloc.free_resource(r).unwrap();
    assert_eq!(alloc.generate_report().allocations.len(), 0);
    assert_no_d3d12_errors(&td.device);
}

// 2
#[test]
fn all_mode_guards_buffer() {
    let td = make_device();
    let mut alloc = stomp_allocator(&td, settings(&td));
    let r = create_buffer(&mut alloc, "stomp", 4000, MemoryLocation::GpuOnly, None);
    // Payload lives in a normal sub-allocator block.
    assert!(r.allocation.is_some());
    assert!(r.stomp_layout().is_some());
    assert!(r.is_stomp_guarded());
    assert_eq!(alloc.generate_report().allocations.len(), 1);
    let stats = alloc.stomp_statistics().unwrap();
    assert_eq!((stats.guarded, stats.unguarded_fallbacks), (1, 0));
    alloc.free_resource(r).unwrap();
    assert_no_d3d12_errors(&td.device);
}

// 3
#[test]
fn width_and_size() {
    let td = make_device();
    let mut alloc = stomp_allocator(&td, settings(&td));
    let r = create_buffer(&mut alloc, "tail", 4000, MemoryLocation::GpuOnly, None);
    assert_eq!(unsafe { r.resource().GetDesc() }.Width, 2 * TILE);
    assert_eq!(r.size, TILE);
    assert_eq!(r.stomp_layout().unwrap().payload_tiles, 1);
    alloc.free_resource(r).unwrap();

    let mut both = stomp_allocator(
        &td,
        StompSettings {
            head_guard: true,
            ..settings(&td)
        },
    );
    let r = create_buffer(&mut both, "head+tail", 65537, MemoryLocation::GpuOnly, None);
    assert_eq!(unsafe { r.resource().GetDesc() }.Width, 4 * TILE);
    assert_eq!(r.size, 2 * TILE);
    assert_eq!(r.stomp_layout().unwrap().payload_tiles, 2);
    both.free_resource(r).unwrap();
    assert_no_d3d12_errors(&td.device);
}

// 4
#[test]
fn tail_guard_va() {
    let td = make_device();
    let mut alloc = stomp_allocator(&td, settings(&td));
    let r = create_buffer(&mut alloc, "tail", 4000, MemoryLocation::GpuOnly, None);
    let layout = r.stomp_layout().unwrap();
    assert_eq!(layout.resource_va, va(&r));
    assert_ne!(layout.resource_va, 0);
    assert_eq!(layout.offset, 0);
    assert_eq!(r.offset(), 0);
    assert_eq!(layout.tail_guard_va(), Some(va(&r) + TILE));
    assert_eq!(layout.head_guard_va(), None);
    alloc.free_resource(r).unwrap();
    assert_no_d3d12_errors(&td.device);
}

// 5
#[test]
fn payload_alignment_packs_against_tail() {
    let td = make_device();
    for (alignment, expected_offset) in [(None, 0), (Some(8), 61536), (Some(256), 61440)] {
        let mut alloc = stomp_allocator(
            &td,
            StompSettings {
                payload_alignment: alignment,
                ..settings(&td)
            },
        );
        let r = create_buffer(&mut alloc, "packed", 4000, MemoryLocation::GpuOnly, None);
        let layout = r.stomp_layout().unwrap();
        assert_eq!(r.offset(), expected_offset, "{alignment:?}");
        assert_eq!(layout.offset, expected_offset);
        let payload_end = va(&r) + r.offset() + 4000;
        let tail = layout.tail_guard_va().unwrap();
        assert!(payload_end <= tail);
        let slack = tail - payload_end;
        match alignment {
            Some(a) => assert!(slack < a, "slack {slack} >= {a}"),
            None => assert_eq!(slack, TILE - 4000),
        }
        alloc.free_resource(r).unwrap();
    }
    assert_no_d3d12_errors(&td.device);
}

// 6
#[test]
fn head_guard_va() {
    let td = make_device();
    let mut alloc = stomp_allocator(
        &td,
        StompSettings {
            head_guard: true,
            ..settings(&td)
        },
    );
    let r = create_buffer(&mut alloc, "head+tail", 4000, MemoryLocation::GpuOnly, None);
    let layout = r.stomp_layout().unwrap();
    assert_eq!(r.offset(), TILE);
    assert_eq!(layout.head_guard_va(), Some(va(&r)));
    assert_eq!(layout.tail_guard_va(), Some(va(&r) + 2 * TILE));
    assert!(r.is_stomp_guarded());
    alloc.free_resource(r).unwrap();
    assert_no_d3d12_errors(&td.device);
}

// 7
#[test]
fn head_only() {
    let td = make_device();
    let mut alloc = stomp_allocator(
        &td,
        StompSettings {
            head_guard: true,
            tail_guard: false,
            payload_alignment: Some(8),
            ..settings(&td)
        },
    );
    let r = create_buffer(&mut alloc, "head", 4000, MemoryLocation::GpuOnly, None);
    let layout = r.stomp_layout().unwrap();
    // No tail guard, so payload_alignment has nothing to pack against.
    assert_eq!(r.offset(), TILE);
    assert_eq!(layout.head_guard_va(), Some(va(&r)));
    assert_eq!(layout.tail_guard_va(), None);
    assert_eq!(unsafe { r.resource().GetDesc() }.Width, 2 * TILE);
    alloc.free_resource(r).unwrap();
    assert_no_d3d12_errors(&td.device);
}

fn count_stomped(td: &TestDevice, probability: f32, seed: u64, n: usize) -> Vec<bool> {
    let mut alloc = stomp_allocator(
        td,
        StompSettings {
            mode: StompMode::Random { probability },
            seed,
            ..settings(td)
        },
    );
    let resources: Vec<_> = (0..n)
        .map(|i| {
            create_buffer(
                &mut alloc,
                &format!("r{i}"),
                1024,
                MemoryLocation::GpuOnly,
                None,
            )
        })
        .collect();
    let stomped = resources
        .iter()
        .map(|r| r.stomp_layout().is_some())
        .collect();
    for r in resources {
        alloc.free_resource(r).unwrap();
    }
    assert_no_d3d12_errors(&td.device);
    stomped
}

// 8
#[test]
fn random_zero_guards_none() {
    let td = make_device();
    assert!(count_stomped(&td, 0.0, 1, 50).iter().all(|&s| !s));
}

// 8
#[test]
fn random_one_guards_all() {
    let td = make_device();
    assert!(count_stomped(&td, 1.0, 1, 50).iter().all(|&s| s));
}

// 9
#[test]
fn random_is_deterministic() {
    let td = make_device();
    let a = count_stomped(&td, 0.5, 42, 100);
    let b = count_stomped(&td, 0.5, 42, 100);
    assert_eq!(a, b);
    assert!(
        a.iter().any(|&s| s) && a.iter().any(|&s| !s),
        "0.5 should mix"
    );
}

// 10
#[test]
fn opt_in_mode() {
    let td = make_device();
    let mut alloc = stomp_allocator(
        &td,
        StompSettings {
            mode: StompMode::OptIn,
            ..settings(&td)
        },
    );
    let normal = create_buffer(&mut alloc, "none", 1024, MemoryLocation::GpuOnly, None);
    let forced = create_buffer(
        &mut alloc,
        "some",
        1024,
        MemoryLocation::GpuOnly,
        Some(true),
    );
    assert!(normal.stomp_layout().is_none());
    assert!(forced.stomp_layout().is_some());
    alloc.free_resource(normal).unwrap();
    alloc.free_resource(forced).unwrap();

    let mut all = stomp_allocator(&td, settings(&td));
    let opted_out = create_buffer(&mut all, "out", 1024, MemoryLocation::GpuOnly, Some(false));
    assert!(opted_out.stomp_layout().is_none());
    all.free_resource(opted_out).unwrap();
    assert_no_d3d12_errors(&td.device);
}

// 11
#[test]
fn filter_mode() {
    let td = make_device();
    let mut alloc = stomp_allocator(
        &td,
        StompSettings {
            mode: StompMode::Filter(|d| d.resource_category == ResourceCategory::Buffer),
            ..settings(&td)
        },
    );
    let buffer = create_buffer(&mut alloc, "buf", 1024, MemoryLocation::GpuOnly, None);
    let tex = create_tex(&mut alloc, "tex", &tex2d_desc(D3D12_RESOURCE_FLAG_NONE, 1));
    assert!(buffer.stomp_layout().is_some());
    assert!(tex.stomp_layout().is_none());
    alloc.free_resource(buffer).unwrap();
    alloc.free_resource(tex).unwrap();
    assert_no_d3d12_errors(&td.device);
}

fn texture_stomped(
    td: &TestDevice,
    flags: D3D12_RESOURCE_FLAGS,
    clear: Option<&D3D12_CLEAR_VALUE>,
) {
    let mut alloc = stomp_allocator(td, settings(td));
    let desc = tex2d_desc(flags, 1);
    let r = create(
        &mut alloc,
        "tex",
        &desc,
        MemoryLocation::GpuOnly,
        None,
        &ResourceType::Placed,
        clear,
    );
    let got = unsafe { r.resource().GetDesc() };
    assert_eq!((got.Width, got.Height, got.Flags), (256, 256, flags));
    assert_eq!(got.Layout, D3D12_TEXTURE_LAYOUT_64KB_UNDEFINED_SWIZZLE);
    let layout = r.stomp_layout().unwrap();
    // Textures get no guard tiles, only quarantine.
    assert!(!r.is_stomp_guarded());
    assert!(!layout.head_guard && !layout.tail_guard);
    assert_eq!(layout.resource_va, 0);
    assert_eq!(layout.offset, 0);
    // 256x256 RGBA8 is exactly 4 tiles.
    assert_eq!(layout.payload_tiles, 4);
    assert_eq!(r.size, 4 * TILE);
    assert!(r.allocation.is_some());
    assert_eq!(alloc.stomp_statistics().unwrap().guarded, 1);
    alloc.free_resource(r).unwrap();
    assert_no_d3d12_errors(&td.device);
}

// 12
#[test]
fn texture_2d_stomped() {
    let td = make_device();
    texture_stomped(&td, D3D12_RESOURCE_FLAG_NONE, None);
}

// 13
#[test]
fn rt_texture_with_clear_value_stomped() {
    let td = make_device();
    let clear = D3D12_CLEAR_VALUE {
        Format: DXGI_FORMAT_R8G8B8A8_UNORM,
        Anonymous: D3D12_CLEAR_VALUE_0 {
            Color: [0.0, 0.5, 1.0, 1.0],
        },
    };
    texture_stomped(&td, D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET, Some(&clear));
}

// 14
#[test]
fn msaa_falls_back() {
    let td = make_device();
    let mut alloc = stomp_allocator(&td, settings(&td));
    let desc = tex2d_desc(D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET, 4);
    let r = create_tex(&mut alloc, "msaa", &desc);
    assert!(r.allocation.is_some() && r.stomp_layout().is_none());
    let stats = alloc.stomp_statistics().unwrap();
    assert_eq!((stats.guarded, stats.unguarded_fallbacks), (0, 1));
    alloc.free_resource(r).unwrap();
    assert_no_d3d12_errors(&td.device);
}

// 15
#[test]
fn texture_3d_tier_dependent() {
    let td = make_device();
    let tier = tiled_tier(&td.device);
    let mut alloc = stomp_allocator(&td, settings(&td));
    let desc = tex_desc(
        D3D12_RESOURCE_DIMENSION_TEXTURE3D,
        64,
        64,
        64,
        DXGI_FORMAT_R8G8B8A8_UNORM,
        D3D12_RESOURCE_FLAG_NONE,
        1,
    );
    let r = create_tex(&mut alloc, "tex3d", &desc);
    let stats = alloc.stomp_statistics().unwrap();
    if tier.0 >= D3D12_TILED_RESOURCES_TIER_3.0 {
        println!("tiled tier {}: 3D stomped", tier.0);
        assert!(r.stomp_layout().is_some());
        // 64^3 RGBA8 = 1MB = 16 tiles.
        assert_eq!(r.stomp_layout().unwrap().payload_tiles, 16);
        assert_eq!((stats.guarded, stats.unguarded_fallbacks), (1, 0));
    } else {
        println!("tiled tier {}: 3D falls back", tier.0);
        assert!(r.stomp_layout().is_none() && r.allocation.is_some());
        assert_eq!((stats.guarded, stats.unguarded_fallbacks), (0, 1));
    }
    alloc.free_resource(r).unwrap();
    assert_no_d3d12_errors(&td.device);
}

// 16
#[test]
fn cpu_heaps_fall_back_and_stay_mappable() {
    // Reserved resources cannot be `Map()`ed (E_INVALIDARG), so CPU-visible resources are
    // refused by the stomp path and must still map through the normal path.
    let td = make_device();
    let mut alloc = stomp_allocator(&td, settings(&td));
    for (i, (name, location)) in [
        ("upload", MemoryLocation::CpuToGpu),
        ("readback", MemoryLocation::GpuToCpu),
    ]
    .into_iter()
    .enumerate()
    {
        let r = create_buffer(&mut alloc, name, 4000, location, None);
        assert!(r.stomp_layout().is_none() && r.allocation.is_some());
        assert_eq!(alloc.stomp_statistics().unwrap().unguarded_fallbacks, i + 1);
        let mut ptr = std::ptr::null_mut();
        unsafe { r.resource().Map(0, None, Some(&mut ptr)) }.unwrap();
        assert!(!ptr.is_null());
        let bytes = unsafe { std::slice::from_raw_parts_mut(ptr.cast::<u8>(), 4000) };
        bytes.fill(0xAB);
        assert_eq!(bytes[3999], 0xAB);
        unsafe { r.resource().Unmap(0, None) };
        alloc.free_resource(r).unwrap();
    }
    assert_no_d3d12_errors(&td.device);
}

// 17
#[test]
fn committed_request_takes_stomp_path() {
    let td = make_device();
    let mut alloc = stomp_allocator(&td, settings(&td));
    let heap_properties = D3D12_HEAP_PROPERTIES {
        Type: D3D12_HEAP_TYPE_DEFAULT,
        ..Default::default()
    };
    let r = create(
        &mut alloc,
        "committed",
        &buffer_desc(4000),
        MemoryLocation::GpuOnly,
        None,
        &ResourceType::Committed {
            heap_properties: &heap_properties,
            heap_flags: D3D12_HEAP_FLAG_NONE,
        },
        None,
    );
    assert!(r.is_stomp_guarded());
    assert!(r.allocation.is_some());
    for s in alloc.committed_statistics() {
        assert_eq!(s.num_allocations, 0);
    }
    alloc.free_resource(r).unwrap();
    assert_no_d3d12_errors(&td.device);
}

fn free_many(td: &TestDevice, quarantine: bool) {
    let mut alloc = stomp_allocator(
        td,
        StompSettings {
            quarantine,
            ..settings(td)
        },
    );
    let resources: Vec<_> = (0..20)
        .map(|i| {
            create_buffer(
                &mut alloc,
                &format!("b{i}"),
                4000,
                MemoryLocation::GpuOnly,
                None,
            )
        })
        .collect();
    let tex = create_tex(&mut alloc, "tex", &tex2d_desc(D3D12_RESOURCE_FLAG_NONE, 1));
    assert_eq!(alloc.generate_report().allocations.len(), 21);
    assert_eq!(alloc.stomp_statistics().unwrap().quarantined, 0);
    for r in resources {
        alloc.free_resource(r).unwrap();
    }
    alloc.free_resource(tex).unwrap();
    assert_eq!(alloc.generate_report().allocations.len(), 0);
    let stats = alloc.stomp_statistics().unwrap();
    assert_eq!(stats.guarded, 21);
    assert_eq!(stats.quarantined, if quarantine { 21 } else { 0 });
    for s in alloc.committed_statistics() {
        assert_eq!((s.num_allocations, s.total_size), (0, 0));
    }
    drop(alloc);
    assert_no_d3d12_errors(&td.device);
}

// 18
#[test]
fn free_with_quarantine() {
    let td = make_device();
    free_many(&td, true);
}

// 19
#[test]
fn free_without_quarantine() {
    let td = make_device();
    free_many(&td, false);
}

fn churn(td: &TestDevice, iterations: usize) {
    let mut alloc = stomp_allocator(td, settings(td));
    let mut ring = std::collections::VecDeque::new();
    let mut seed = 0x1234_5678_u64;
    for i in 0..iterations {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let width = 1 + seed % (4 * 1024 * 1024);
        let r = create_buffer(
            &mut alloc,
            &format!("churn{i}"),
            width,
            MemoryLocation::GpuOnly,
            None,
        );
        assert!(r.is_stomp_guarded(), "iteration {i}");
        let layout = r.stomp_layout().unwrap();
        assert_eq!(
            layout.tail_guard_va(),
            Some(va(&r) + u64::from(layout.payload_tiles) * TILE)
        );
        ring.push_back(r);
        if ring.len() > 32 {
            alloc.free_resource(ring.pop_front().unwrap()).unwrap();
        }
    }
    for r in ring {
        alloc.free_resource(r).unwrap();
    }
    let stats = alloc.stomp_statistics().unwrap();
    println!("churn: {stats:?}");
    assert_eq!(
        (stats.guarded, stats.unguarded_fallbacks, stats.quarantined),
        (iterations, 0, iterations)
    );
    assert_eq!(alloc.generate_report().allocations.len(), 0);
}

// 20
#[test]
fn churn_all_guarded() {
    let td = make_device();
    churn(&td, 500);
    assert_no_d3d12_errors(&td.device);
}

// 21
#[test]
fn churn_from_threads() {
    let handles: Vec<_> = (0..4)
        .map(|_| {
            std::thread::spawn(|| {
                let td = make_device();
                churn(&td, 100);
                assert_no_d3d12_errors(&td.device);
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
}

// 22
#[test]
fn drop_order_is_safe() {
    let td = make_device();
    {
        let mut alloc = stomp_allocator(&td, settings(&td));
        let r = create_buffer(&mut alloc, "freed", 4000, MemoryLocation::GpuOnly, None);
        alloc.free_resource(r).unwrap();
    }
    {
        let mut alloc = stomp_allocator(&td, settings(&td));
        let live = create_buffer(&mut alloc, "live", 4000, MemoryLocation::GpuOnly, None);
        drop(alloc);
        drop(live); // warns "not freed", must not crash
    }
    assert_no_d3d12_errors(&td.device);
}

// 23
#[test]
fn rename_and_report_unaffected() {
    let td = make_device();
    let mut alloc = stomp_allocator(&td, settings(&td));
    let desc = buffer_desc(1024);
    let mut allocation = alloc
        .allocate(&AllocationCreateDesc::from_d3d12_resource_desc(
            &td.device,
            &desc,
            "before",
            MemoryLocation::GpuOnly,
        ))
        .unwrap();
    alloc.rename_allocation(&mut allocation, "after").unwrap();
    assert_eq!(alloc.generate_report().allocations[0].name, "after");
    alloc.report_memory_leaks(log::Level::Debug);
    alloc.free(allocation).unwrap();
    assert_no_d3d12_errors(&td.device);
}

// 24
#[test]
fn invalid_settings_rejected() {
    let td = make_device();
    let invalid = |s: StompSettings| {
        matches!(
            try_allocator(&td, Some(s)),
            Err(AllocationError::InvalidStompSettings(_))
        )
    };
    assert!(invalid(StompSettings {
        mode: StompMode::Random { probability: 1.5 },
        ..settings(&td)
    }));
    assert!(invalid(StompSettings {
        tail_guard: false,
        head_guard: false,
        ..settings(&td)
    }));
    assert!(invalid(StompSettings {
        payload_alignment: Some(3),
        ..settings(&td)
    }));
    assert!(try_allocator(&td, Some(settings(&td))).is_ok());
    assert_no_d3d12_errors(&td.device);
}

// 25
#[test]
fn tiled_tier_unsupported_rejected() {
    let td = make_device();
    let tier = tiled_tier(&td.device);
    if tier.0 >= D3D12_TILED_RESOURCES_TIER_1.0 {
        println!("tiled tier {}, nothing to test", tier.0);
        return;
    }
    assert!(matches!(
        try_allocator(&td, Some(settings(&td))),
        Err(AllocationError::InvalidStompSettings(_))
    ));
}
