#![allow(clippy::unwrap_used)]
use super::*;
use alloc::collections::{BTreeMap, VecDeque};
use ash::vk::Handle;
use core::cell::RefCell;

const MIB: u64 = 1024 * 1024;

#[derive(Default)]
pub(super) struct Driver {
    pub(super) calls: Vec<(u32, u64)>,
    pub(super) dedicated: Vec<Option<(vk::Buffer, vk::Image)>>,
    pub(super) allocation_flags: Vec<vk::MemoryAllocateFlags>,
    pub(super) freed: usize,
    pub(super) fail: bool,
    pub(super) limit: Option<u64>,
    pub(super) live: BTreeMap<u64, u64>,
    pub(super) errors: VecDeque<vk::Result>,
    pub(super) map_errors: VecDeque<vk::Result>,
    pub(super) map_fail: Option<vk::Result>,
    pub(super) mappings: BTreeMap<u64, Vec<u8>>,
    pub(super) null_map: bool,
    pub(super) unmapped: usize,
    pub(super) heaps: Vec<(u64, vk::MemoryPropertyFlags)>,
    pub(super) max_allocations: u32,
}
thread_local! {
    pub(super) static DRIVER: RefCell<Driver> = RefCell::new(Driver::default());
}
unsafe extern "system" fn allocate(
    _: vk::Device,
    info: *const vk::MemoryAllocateInfo<'_>,
    _: *const vk::AllocationCallbacks<'_>,
    memory: *mut vk::DeviceMemory,
) -> vk::Result {
    DRIVER.with(|state| {
        let mut state = state.borrow_mut();
        let info = unsafe { &*info };
        state
            .calls
            .push((info.memory_type_index, info.allocation_size));
        let mut dedicated = None;
        let mut flags = vk::MemoryAllocateFlags::empty();
        let mut next = info.p_next.cast::<vk::BaseInStructure<'_>>();
        while !next.is_null() {
            let base = unsafe { &*next };
            match base.s_type {
                vk::StructureType::MEMORY_DEDICATED_ALLOCATE_INFO => {
                    let info = unsafe { &*next.cast::<vk::MemoryDedicatedAllocateInfo<'_>>() };
                    dedicated = Some((info.buffer, info.image));
                }
                vk::StructureType::MEMORY_ALLOCATE_FLAGS_INFO => {
                    flags = unsafe { (*next.cast::<vk::MemoryAllocateFlagsInfo<'_>>()).flags };
                }
                _ => {}
            }
            next = base.p_next;
        }
        state.dedicated.push(dedicated);
        state.allocation_flags.push(flags);
        if let Some(error) = state.errors.pop_front() {
            return error;
        }
        let used: u64 = state.live.values().sum();
        if state.fail
            || state
                .limit
                .is_some_and(|limit| used + info.allocation_size > limit)
        {
            vk::Result::ERROR_OUT_OF_DEVICE_MEMORY
        } else {
            let id = state.calls.len() as u64;
            unsafe { *memory = vk::DeviceMemory::from_raw(id) };
            state.live.insert(id, info.allocation_size);
            vk::Result::SUCCESS
        }
    })
}
unsafe extern "system" fn free(
    _: vk::Device,
    memory: vk::DeviceMemory,
    _: *const vk::AllocationCallbacks<'_>,
) {
    DRIVER.with(|state| {
        let mut state = state.borrow_mut();
        assert!(state.live.remove(&memory.as_raw()).is_some());
        assert!(!state.mappings.contains_key(&memory.as_raw()));
        state.freed += 1;
    });
}
unsafe extern "system" fn map(
    _: vk::Device,
    memory: vk::DeviceMemory,
    _: u64,
    _: u64,
    _: vk::MemoryMapFlags,
    pointer: *mut *mut core::ffi::c_void,
) -> vk::Result {
    DRIVER.with(|state| {
        let mut state = state.borrow_mut();
        if let Some(error) = state.map_errors.pop_front().or(state.map_fail) {
            return error;
        }
        let ptr = if state.null_map {
            core::ptr::null_mut()
        } else {
            let size = state.live[&memory.as_raw()] as usize;
            state.mappings.insert(memory.as_raw(), vec![0; size]);
            state
                .mappings
                .get_mut(&memory.as_raw())
                .unwrap()
                .as_mut_ptr()
                .cast()
        };
        unsafe {
            *pointer = ptr;
        }
        vk::Result::SUCCESS
    })
}
unsafe extern "system" fn unmap(_: vk::Device, memory: vk::DeviceMemory) {
    DRIVER.with(|state| {
        let mut state = state.borrow_mut();
        state.mappings.remove(&memory.as_raw());
        state.unmapped += 1;
    });
}

unsafe extern "system" fn memory_properties(
    _: vk::PhysicalDevice,
    out: *mut vk::PhysicalDeviceMemoryProperties,
) {
    let mut props = vk::PhysicalDeviceMemoryProperties::default();
    DRIVER.with(|d| {
        let d = d.borrow();
        props.memory_type_count = d.heaps.len() as u32;
        props.memory_heap_count = d.heaps.len() as u32;
        for (i, &(size, flags)) in d.heaps.iter().enumerate() {
            props.memory_types[i] = vk::MemoryType {
                property_flags: flags,
                heap_index: i as u32,
            };
            props.memory_heaps[i] = vk::MemoryHeap {
                size,
                flags: if flags.contains(vk::MemoryPropertyFlags::DEVICE_LOCAL) {
                    vk::MemoryHeapFlags::DEVICE_LOCAL
                } else {
                    vk::MemoryHeapFlags::empty()
                },
            };
        }
    });
    unsafe {
        *out = props;
    }
}
unsafe extern "system" fn device_properties(
    _: vk::PhysicalDevice,
    out: *mut vk::PhysicalDeviceProperties,
) {
    let mut props = vk::PhysicalDeviceProperties::default();
    props.limits.buffer_image_granularity = 1;
    props.limits.max_memory_allocation_count = DRIVER.with(|d| d.borrow().max_allocations);
    unsafe {
        *out = props;
    }
}
pub(super) fn fixture(heaps: &[(u64, vk::MemoryPropertyFlags)], fail: bool) -> Allocator {
    fixture_with_limit(heaps, fail, 4096)
}
pub(super) fn fixture_with_limit(
    heaps: &[(u64, vk::MemoryPropertyFlags)],
    fail: bool,
    count: u32,
) -> Allocator {
    DRIVER.with(|d| {
        *d.borrow_mut() = Driver {
            fail,
            heaps: heaps.to_vec(),
            max_allocations: count,
            ..Default::default()
        }
    });
    let instance = unsafe {
        ash::Instance::load_with(
            |name| match name.to_bytes() {
                b"vkGetPhysicalDeviceMemoryProperties" => (memory_properties as *const ()).cast(),
                b"vkGetPhysicalDeviceProperties" => (device_properties as *const ()).cast(),
                _ => core::ptr::null(),
            },
            vk::Instance::null(),
        )
    };
    let device = unsafe {
        ash::Device::load_with(
            |name| match name.to_bytes() {
                b"vkAllocateMemory" => (allocate as *const ()).cast(),
                b"vkFreeMemory" => (free as *const ()).cast(),
                b"vkMapMemory" => (map as *const ()).cast(),
                b"vkUnmapMemory" => (unmap as *const ()).cast(),
                _ => core::ptr::null(),
            },
            vk::Device::null(),
        )
    };
    Allocator::new(&AllocatorCreateDesc {
        instance,
        device,
        physical_device: vk::PhysicalDevice::from_raw(1),
        debug_settings: AllocatorDebugSettings::default(),
        buffer_device_address: false,
        allocation_sizes: AllocationSizes::new(8 * MIB, 8 * MIB),
    })
    .unwrap()
}
pub(super) fn request(size: u64, location: MemoryLocation) -> AllocationCreateDesc<'static> {
    AllocationCreateDesc {
        name: "contract test",
        requirements: vk::MemoryRequirements {
            size,
            alignment: 1,
            memory_type_bits: u32::MAX,
        },
        location,
        linear: true,
        allocation_scheme: AllocationScheme::GpuAllocatorManaged,
    }
}
