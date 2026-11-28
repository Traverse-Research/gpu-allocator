#![allow(clippy::unwrap_used)]

use super::*;
use alloc::collections::VecDeque;
use core::cell::RefCell;
use core::{
    cell::Cell,
    ffi::c_void,
    mem::{size_of, MaybeUninit},
};
use std::sync::OnceLock;
use windows::core::{Interface, GUID, HRESULT};

const MIB: u64 = 1024 * 1024;

#[derive(Default)]
struct Driver {
    calls: Vec<u64>,
    errors: VecDeque<HRESULT>,
    live_bytes: u64,
    live_heaps: usize,
    count_limit: Option<usize>,
    destroyed: usize,
    limit: Option<u64>,
    resource_size: u64,
    resource_error: Option<HRESULT>,
    resource_null: bool,
    resource_calls: usize,
    live_resources: usize,
    destruction_order: Vec<&'static str>,
}
thread_local! { static DRIVER: RefCell<Driver> = RefCell::new(Driver::default()); }

#[repr(C)]
struct Object {
    vtable: *const usize,
    refs: Cell<u32>,
    heap_size: u64,
    is_resource: bool,
}

unsafe extern "system" fn query(_: *mut c_void, _: *const GUID, _: *mut *mut c_void) -> HRESULT {
    HRESULT(0x80004002_u32 as i32)
}
unsafe extern "system" fn add_ref(this: *mut c_void) -> u32 {
    let object = unsafe { &*this.cast::<Object>() };
    object.refs.set(object.refs.get() + 1);
    object.refs.get()
}
unsafe extern "system" fn release(this: *mut c_void) -> u32 {
    let object = unsafe { &*this.cast::<Object>() };
    let refs = object.refs.get() - 1;
    object.refs.set(refs);
    if refs == 0 {
        let object = unsafe { Box::from_raw(this.cast::<Object>()) };
        if object.is_resource {
            DRIVER.with(|d| {
                let mut d = d.borrow_mut();
                d.live_resources -= 1;
                d.destruction_order.push("resource");
            });
        }
        if object.heap_size != 0 {
            DRIVER.with(|d| {
                let mut d = d.borrow_mut();
                d.live_bytes -= object.heap_size;
                d.live_heaps -= 1;
                d.destroyed += 1;
                d.destruction_order.push("heap");
            });
        }
    }
    refs
}
fn unknown_table() -> &'static [usize] {
    static TABLE: OnceLock<Vec<usize>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table =
            vec![query as *const () as usize; size_of::<ID3D12Heap_Vtbl>() / size_of::<usize>()];
        table[..3].copy_from_slice(&[
            query as *const () as usize,
            add_ref as *const () as usize,
            release as *const () as usize,
        ]);
        table
    })
}
unsafe extern "system" fn create_heap(
    _: *mut c_void,
    desc: *const D3D12_HEAP_DESC,
    _: *const GUID,
    heap: *mut *mut c_void,
) -> HRESULT {
    let size = unsafe { (*desc).SizeInBytes };
    DRIVER.with(|d| {
        let mut d = d.borrow_mut();
        d.calls.push(size);
        if let Some(error) = d.errors.pop_front() {
            return error;
        }
        if d.limit.is_some_and(|limit| d.live_bytes + size > limit)
            || d.count_limit.is_some_and(|limit| d.live_heaps >= limit)
        {
            return E_OUTOFMEMORY;
        }
        d.live_bytes += size;
        d.live_heaps += 1;
        let object = Box::new(Object {
            vtable: unknown_table().as_ptr(),
            refs: Cell::new(1),
            heap_size: size,
            is_resource: false,
        });
        unsafe {
            *heap = Box::into_raw(object).cast();
        }
        HRESULT(0)
    })
}

unsafe extern "system" fn allocation_info(
    _: *mut c_void,
    info: *mut D3D12_RESOURCE_ALLOCATION_INFO,
    _: u32,
    _: u32,
    _: *const D3D12_RESOURCE_DESC,
) {
    let size = DRIVER.with(|d| d.borrow().resource_size.max(MIB));
    unsafe {
        *info = D3D12_RESOURCE_ALLOCATION_INFO {
            SizeInBytes: size,
            Alignment: 65536,
        };
    }
}

fn create_resource_output(output: *mut *mut c_void) -> HRESULT {
    static TABLE: OnceLock<Vec<usize>> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut table = vec![
            query as *const () as usize;
            size_of::<ID3D12Resource_Vtbl>() / size_of::<usize>()
        ];
        table[..3].copy_from_slice(&unknown_table()[..3]);
        table
    });
    DRIVER.with(|d| {
        let mut d = d.borrow_mut();
        d.resource_calls += 1;
        if !d.resource_null {
            d.live_resources += 1;
            let object = Box::new(Object {
                vtable: table.as_ptr(),
                refs: Cell::new(1),
                heap_size: 0,
                is_resource: true,
            });
            unsafe { *output = Box::into_raw(object).cast() };
        }
        d.resource_error.unwrap_or(HRESULT(0))
    })
}

unsafe extern "system" fn create_placed_resource(
    _: *mut c_void,
    _: *mut c_void,
    _: u64,
    _: *const D3D12_RESOURCE_DESC,
    _: D3D12_RESOURCE_STATES,
    _: *const D3D12_CLEAR_VALUE,
    _: *const GUID,
    output: *mut *mut c_void,
) -> HRESULT {
    create_resource_output(output)
}

unsafe extern "system" fn create_committed_resource(
    _: *mut c_void,
    _: *const D3D12_HEAP_PROPERTIES,
    _: D3D12_HEAP_FLAGS,
    _: *const D3D12_RESOURCE_DESC,
    _: D3D12_RESOURCE_STATES,
    _: *const D3D12_CLEAR_VALUE,
    _: *const GUID,
    output: *mut *mut c_void,
) -> HRESULT {
    create_resource_output(output)
}

unsafe extern "system" fn check_feature_support(
    _: *mut c_void,
    feature: D3D12_FEATURE,
    output: *mut c_void,
    size: u32,
) -> HRESULT {
    if feature != D3D12_FEATURE_D3D12_OPTIONS
        || size as usize != size_of::<D3D12_FEATURE_DATA_D3D12_OPTIONS>()
    {
        return HRESULT(0x80070057_u32 as i32);
    }
    unsafe {
        *output.cast::<D3D12_FEATURE_DATA_D3D12_OPTIONS>() = D3D12_FEATURE_DATA_D3D12_OPTIONS {
            ResourceHeapTier: D3D12_RESOURCE_HEAP_TIER_2,
            ..Default::default()
        };
    }
    HRESULT(0)
}

fn fixture() -> Allocator {
    DRIVER.with(|d| *d.borrow_mut() = Driver::default());
    // Only the explicitly installed methods are called. Allocate full tables with non-null
    // entries; unused methods are never invoked. Field offsets come from the bindings.
    static DEVICE_TABLE: OnceLock<Vec<usize>> = OnceLock::new();
    let table = DEVICE_TABLE.get_or_init(|| {
        let mut table =
            vec![query as *const () as usize; size_of::<ID3D12Device_Vtbl>() / size_of::<usize>()];
        table[..3].copy_from_slice(&unknown_table()[..3]);
        let layout = MaybeUninit::<ID3D12Device_Vtbl>::uninit();
        let base = layout.as_ptr();
        let offset = unsafe { core::ptr::addr_of!((*base).CreateHeap) as usize - base as usize };
        table[offset / size_of::<usize>()] = create_heap as *const () as usize;
        let offset =
            unsafe { core::ptr::addr_of!((*base).CheckFeatureSupport) as usize - base as usize };
        table[offset / size_of::<usize>()] = check_feature_support as *const () as usize;
        let offset = unsafe {
            core::ptr::addr_of!((*base).GetResourceAllocationInfo) as usize - base as usize
        };
        table[offset / size_of::<usize>()] = allocation_info as *const () as usize;
        let offset =
            unsafe { core::ptr::addr_of!((*base).CreatePlacedResource) as usize - base as usize };
        table[offset / size_of::<usize>()] = create_placed_resource as *const () as usize;
        let offset = unsafe {
            core::ptr::addr_of!((*base).CreateCommittedResource) as usize - base as usize
        };
        table[offset / size_of::<usize>()] = create_committed_resource as *const () as usize;
        table
    });
    let object = Box::new(Object {
        vtable: table.as_ptr(),
        refs: Cell::new(1),
        heap_size: 0,
        is_resource: false,
    });
    let device = unsafe { ID3D12Device::from_raw(Box::into_raw(object).cast()) };
    Allocator::new(&AllocatorCreateDesc {
        device: ID3D12DeviceVersion::Device(device),
        debug_settings: AllocatorDebugSettings::default(),
        allocation_sizes: AllocationSizes::new(8 * MIB, 8 * MIB)
            .with_max_device_memblock_size(64 * MIB),
    })
    .unwrap()
}
fn resource_request(desc: &D3D12_RESOURCE_DESC) -> ResourceCreateDesc<'_> {
    ResourceCreateDesc {
        name: "resource lifetime test",
        memory_location: MemoryLocation::GpuOnly,
        resource_category: ResourceCategory::Buffer,
        resource_desc: desc,
        castable_formats: &[],
        clear_value: None,
        initial_state_or_layout: ResourceStateOrBarrierLayout::ResourceState(
            D3D12_RESOURCE_STATE_COMMON,
        ),
        resource_type: &ResourceType::Placed,
    }
}

#[test]
fn placed_creation_error_drops_partial_resource_before_freeing_backing_memory() {
    let mut allocator = fixture();
    DRIVER.with(|d| {
        let mut d = d.borrow_mut();
        d.resource_size = 65 * MIB;
        d.resource_error = Some(E_OUTOFMEMORY);
    });
    assert!(matches!(
        allocator.create_resource(&resource_request(&D3D12_RESOURCE_DESC::default())),
        Err(AllocationError::Internal(_))
    ));
    assert_eq!(allocator.capacity(), 0);
    assert_eq!(DRIVER.with(|d| d.borrow().live_resources), 0);
    assert_eq!(
        DRIVER.with(|d| d.borrow().destruction_order.clone()),
        vec!["resource", "heap"]
    );
}

#[test]
fn placed_creation_null_success_returns_error_and_releases_allocation() {
    let mut allocator = fixture();
    DRIVER.with(|d| {
        let mut d = d.borrow_mut();
        d.resource_size = 65 * MIB;
        d.resource_null = true;
    });
    assert!(matches!(
        allocator.create_resource(&resource_request(&D3D12_RESOURCE_DESC::default())),
        Err(AllocationError::Internal(_))
    ));
    assert_eq!(allocator.capacity(), 0);
    assert_eq!(DRIVER.with(|d| d.borrow().live_bytes), 0);
}

#[test]
fn unsupported_placed_creation_options_return_the_suballocation() {
    let mut allocator = fixture();
    let native = D3D12_RESOURCE_DESC::default();
    let mut desc = resource_request(&native);
    desc.castable_formats = &[DXGI_FORMAT(28)];
    assert!(matches!(
        allocator.create_resource(&desc),
        Err(AllocationError::CastableFormatsRequiresEnhancedBarriers)
    ));
    desc.initial_state_or_layout =
        ResourceStateOrBarrierLayout::BarrierLayout(D3D12_BARRIER_LAYOUT_COMMON);
    assert!(matches!(
        allocator.create_resource(&desc),
        Err(AllocationError::CastableFormatsRequiresAtLeastDevice12)
    ));
    desc.castable_formats = &[];
    assert!(matches!(
        allocator.create_resource(&desc),
        Err(AllocationError::BarrierLayoutNeedsDevice10)
    ));
    assert_eq!(allocator.generate_report().total_allocated_bytes, 0);
    assert_eq!(DRIVER.with(|d| d.borrow().resource_calls), 0);
    drop(allocator);
    assert_eq!(DRIVER.with(|d| d.borrow().live_bytes), 0);
}

#[test]
fn successful_resource_ownership_is_freed_once() {
    let mut allocator = fixture();
    let resource = allocator
        .create_resource(&resource_request(&D3D12_RESOURCE_DESC::default()))
        .unwrap();
    assert_eq!(allocator.generate_report().total_allocated_bytes, MIB);
    assert_eq!(DRIVER.with(|d| d.borrow().live_resources), 1);
    allocator.free_resource(resource).unwrap();
    assert_eq!(allocator.generate_report().total_allocated_bytes, 0);
    assert_eq!(DRIVER.with(|d| d.borrow().live_resources), 0);
    drop(allocator);
    assert_eq!(DRIVER.with(|d| d.borrow().live_bytes), 0);
    assert_eq!(
        DRIVER.with(|d| d.borrow().destruction_order.clone()),
        vec!["resource", "heap"]
    );
}

#[test]
fn committed_creation_null_success_is_an_error() {
    let mut allocator = fixture();
    DRIVER.with(|d| d.borrow_mut().resource_null = true);
    let native = D3D12_RESOURCE_DESC::default();
    let mut desc = resource_request(&native);
    let properties = D3D12_HEAP_PROPERTIES::default();
    let kind = ResourceType::Committed {
        heap_properties: &properties,
        heap_flags: D3D12_HEAP_FLAG_NONE,
    };
    desc.resource_type = &kind;
    assert!(matches!(
        allocator.create_resource(&desc),
        Err(AllocationError::Internal(_))
    ));
    assert_eq!(allocator.capacity(), 0);
}
