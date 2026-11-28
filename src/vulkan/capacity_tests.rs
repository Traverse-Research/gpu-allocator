#![allow(clippy::unwrap_used)]
use super::*;
use alloc::collections::{BTreeMap, VecDeque};
use ash::vk::Handle;
use core::cell::RefCell;

const MIB: u64 = 1024 * 1024;

#[derive(Default)]
struct Driver {
    calls: Vec<(u32, u64)>,
    dedicated: Vec<Option<(vk::Buffer, vk::Image)>>,
    allocation_flags: Vec<vk::MemoryAllocateFlags>,
    freed: usize,
    fail: bool,
    limit: Option<u64>,
    live: BTreeMap<u64, u64>,
    errors: VecDeque<vk::Result>,
    map_errors: VecDeque<vk::Result>,
    map_fail: Option<vk::Result>,
    mappings: BTreeMap<u64, Vec<u8>>,
    null_map: bool,
    unmapped: usize,
}
thread_local! {
    static DRIVER: RefCell<Driver> = RefCell::new(Driver::default());
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
fn fixture(heaps: &[(u64, vk::MemoryPropertyFlags)], fail: bool) -> Allocator {
    DRIVER.with(|state| {
        *state.borrow_mut() = Driver {
            fail,
            ..Default::default()
        }
    });
    let device = unsafe {
        ash::Device::load_with(
            |name| match name.to_bytes() {
                b"vkAllocateMemory" => (allocate as *const ()).cast::<core::ffi::c_void>(),
                b"vkMapMemory" => (map as *const ()).cast::<core::ffi::c_void>(),
                b"vkUnmapMemory" => (unmap as *const ()).cast::<core::ffi::c_void>(),
                b"vkFreeMemory" => (free as *const ()).cast::<core::ffi::c_void>(),
                _ => core::ptr::null(),
            },
            vk::Device::null(),
        )
    };
    let memory_types = heaps
        .iter()
        .enumerate()
        .map(|(i, &(size, flags))| MemoryType {
            memory_blocks: Vec::new(),
            memory_properties: flags,
            memory_type_index: i,
            heap_index: i,
            heap_size: size,
            // These tests intercept API allocation without exposing CPU pointers.
            mappable: false,
            active_general_blocks: 0,
            active_blocks: 0,
            empty_block_index: None,
            block_sizes: AllocationSizes::new(256 * MIB, 256 * MIB)
                .block_sizes_for(flags.contains(vk::MemoryPropertyFlags::HOST_VISIBLE))
                .capped_to_heap(size),
            buffer_device_address: false,
        })
        .collect();
    Allocator {
        memory_types,
        memory_heaps: heaps
            .iter()
            .map(|&(size, properties)| vk::MemoryHeap {
                size,
                flags: if properties.contains(vk::MemoryPropertyFlags::DEVICE_LOCAL) {
                    vk::MemoryHeapFlags::DEVICE_LOCAL
                } else {
                    vk::MemoryHeapFlags::empty()
                },
            })
            .collect(),
        device,
        max_memory_allocation_count: 4096,
        buffer_image_granularity: 1,
        debug_settings: AllocatorDebugSettings::default(),
    }
}
fn request(size: u64, location: MemoryLocation) -> AllocationCreateDesc<'static> {
    AllocationCreateDesc {
        name: "capacity test",
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

#[test]
#[allow(clippy::unwrap_used)]
fn shared_blocks_fit_small_and_non_quantized_heaps() {
    for size in [1, 2 * MIB, 4 * MIB - 1, 214 * MIB + 123] {
        let mut allocator = fixture(&[(size, vk::MemoryPropertyFlags::DEVICE_LOCAL)], false);
        let allocation = allocator
            .allocate(&request(size, MemoryLocation::GpuOnly))
            .unwrap();
        assert_eq!(allocation.size(), size);
        assert_eq!(DRIVER.with(|d| d.borrow().calls.clone()), vec![(0, size)]);
        allocator.free(allocation).unwrap();
        drop(allocator);
        assert_eq!(DRIVER.with(|d| d.borrow().freed), 1);
    }
}

#[test]
fn oversized_resources_never_reach_driver() {
    let mut allocator = fixture(&[(2 * MIB, vk::MemoryPropertyFlags::DEVICE_LOCAL)], false);
    assert!(matches!(
        allocator.allocate(&request(2 * MIB + 1, MemoryLocation::GpuOnly)),
        Err(AllocationError::OutOfMemory)
    ));
    assert!(DRIVER.with(|d| d.borrow().calls.is_empty()));
}

#[test]
fn fallback_resource_capacity_is_checked_before_api_call() {
    let host = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
    let mut allocator = fixture(
        &[
            (8 * MIB, host),
            (64 * MIB, host | vk::MemoryPropertyFlags::DEVICE_LOCAL),
        ],
        true,
    );
    assert!(matches!(
        allocator.allocate(&request(16 * MIB, MemoryLocation::CpuToGpu)),
        Err(AllocationError::OutOfMemory)
    ));
    assert_eq!(
        DRIVER.with(|d| d.borrow().calls.clone()),
        vec![(1, 64 * MIB), (1, 32 * MIB), (1, 16 * MIB)]
    );
}

#[test]
fn fallback_shared_block_is_capped_to_its_own_heap() {
    let host = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
    let mut allocator = fixture(
        &[
            (8 * MIB, host),
            (64 * MIB, host | vk::MemoryPropertyFlags::DEVICE_LOCAL),
        ],
        true,
    );
    assert!(matches!(
        allocator.allocate(&request(MIB, MemoryLocation::CpuToGpu)),
        Err(AllocationError::OutOfMemory)
    ));
    assert_eq!(
        DRIVER.with(|d| d.borrow().calls.clone()),
        vec![
            (1, 64 * MIB),
            (1, 32 * MIB),
            (1, 16 * MIB),
            (1, 8 * MIB),
            (1, 4 * MIB),
            (1, 2 * MIB),
            (1, MIB),
            (0, 8 * MIB),
            (0, 4 * MIB),
            (0, 2 * MIB),
            (0, MIB)
        ]
    );
}

#[test]
#[allow(clippy::unwrap_used)]
fn cached_block_is_reused_while_another_block_remains_live() {
    let mut allocator = fixture(&[(256 * MIB, vk::MemoryPropertyFlags::DEVICE_LOCAL)], false);
    allocator.memory_types[0].block_sizes = AllocationSizes::new(8 * MIB, 4 * MIB)
        .with_max_device_memblock_size(64 * MIB)
        .block_sizes_for(false);
    let anchor = allocator
        .allocate(&request(1024, MemoryLocation::GpuOnly))
        .unwrap();
    for _ in 0..4 {
        let mut burst = Vec::new();
        for _ in 0..8 {
            burst.push(
                allocator
                    .allocate(&request(2 * MIB + MIB / 4, MemoryLocation::GpuOnly))
                    .unwrap(),
            );
        }
        for allocation in burst {
            allocator.free(allocation).unwrap();
        }
        assert!(allocator.memory_types[0].empty_block_index.is_some());
    }
    assert_eq!(DRIVER.with(|d| d.borrow().calls.len()), 2);
    allocator.free(anchor).unwrap();
    assert_eq!(allocator.capacity(), 16 * MIB);
    assert_eq!(allocator.memory_types[0].active_general_blocks, 1);
}

#[test]
#[allow(clippy::unwrap_used)]
fn trim_preserves_live_allocations_and_allows_regrowth() {
    let mut allocator = fixture(&[(256 * MIB, vk::MemoryPropertyFlags::DEVICE_LOCAL)], false);
    allocator.memory_types[0].block_sizes = AllocationSizes::new(8 * MIB, 4 * MIB)
        .with_max_device_memblock_size(64 * MIB)
        .block_sizes_for(false);
    let anchor = allocator
        .allocate(&request(1024, MemoryLocation::GpuOnly))
        .unwrap();
    let mut burst = Vec::new();
    for _ in 0..8 {
        burst.push(
            allocator
                .allocate(&request(2 * MIB + MIB / 4, MemoryLocation::GpuOnly))
                .unwrap(),
        );
    }
    for allocation in burst {
        allocator.free(allocation).unwrap();
    }
    assert_eq!(allocator.trim(), 16 * MIB);
    assert_eq!(allocator.generate_report().total_allocated_bytes, 1024);
    assert_eq!(allocator.memory_types[0].active_general_blocks, 1);
    assert!(allocator.memory_types[0].empty_block_index.is_none());
    let another = allocator
        .allocate(&request(MIB, MemoryLocation::GpuOnly))
        .unwrap();
    allocator.free(another).unwrap();
    allocator.free(anchor).unwrap();
    assert_eq!(allocator.trim(), 8 * MIB);
    assert_eq!(allocator.trim(), 0);
    assert_eq!(allocator.capacity(), 0);
    assert_eq!(allocator.memory_types[0].active_general_blocks, 0);
    let allocation = allocator
        .allocate(&request(MIB, MemoryLocation::GpuOnly))
        .unwrap();
    allocator.free(allocation).unwrap();
    assert_eq!(allocator.trim(), 8 * MIB);
    assert_eq!(DRIVER.with(|d| d.borrow().calls.len()), 3);
}

#[test]
#[allow(clippy::unwrap_used)]
fn cache_invariants_hold_under_churn_and_different_free_orders() {
    let mut allocator = fixture(&[(256 * MIB, vk::MemoryPropertyFlags::DEVICE_LOCAL)], false);
    allocator.memory_types[0].block_sizes = AllocationSizes::new(8 * MIB, 4 * MIB)
        .with_max_device_memblock_size(64 * MIB)
        .block_sizes_for(false);
    for round in 0..32 {
        let mut live = Vec::new();
        for i in 0..20 {
            let size = ((i * 17 + round * 7) % 25 + 1) * MIB / 2;
            live.push(
                allocator
                    .allocate(&request(size, MemoryLocation::GpuOnly))
                    .unwrap(),
            );
        }
        if round % 2 == 0 {
            live.reverse();
        }
        for allocation in live {
            allocator.free(allocation).unwrap();
            let ty = &allocator.memory_types[0];
            let empty: Vec<_> = ty
                .memory_blocks
                .iter()
                .enumerate()
                .filter_map(|(i, block)| {
                    block
                        .as_ref()
                        .filter(|b| b.sub_allocator.is_empty())
                        .map(|b| (i, b.size))
                })
                .collect();
            assert!(empty.len() <= 1);
            if let Some(&(i, size)) = empty.first() {
                assert_eq!(ty.empty_block_index, Some(i));
                assert!(size <= 16 * MIB);
            } else {
                assert!(ty.empty_block_index.is_none());
            }
        }
        assert_eq!(allocator.generate_report().total_allocated_bytes, 0);
        assert_eq!(allocator.memory_types[0].active_general_blocks, 1);
    }
}

#[test]
#[allow(clippy::unwrap_used)]
fn geometric_request_advancement_respects_heap_capacity() {
    let host = vk::MemoryPropertyFlags::HOST_VISIBLE
        | vk::MemoryPropertyFlags::HOST_COHERENT
        | vk::MemoryPropertyFlags::DEVICE_LOCAL;
    let mut allocator = fixture(&[(214 * MIB, host)], false);
    allocator.memory_types[0].block_sizes = AllocationSizes::new(4 * MIB, 4 * MIB)
        .with_max_host_memblock_size(256 * MIB)
        .block_sizes_for(true)
        .capped_to_heap(214 * MIB);
    let allocation = allocator
        .allocate(&request(192 * MIB, MemoryLocation::CpuToGpu))
        .unwrap();
    assert_eq!(
        DRIVER.with(|d| d.borrow().calls.clone()),
        vec![(0, 214 * MIB)]
    );
    allocator.free(allocation).unwrap();
    assert_eq!(allocator.capacity(), 0);
}

fn growable(allocator: &mut Allocator, min: u64, max: u64) {
    for ty in &mut allocator.memory_types {
        ty.block_sizes = AllocationSizes::new(min, min)
            .with_max_device_memblock_size(max)
            .with_max_host_memblock_size(max)
            .block_sizes_for(ty.mappable)
            .capped_to_heap(ty.heap_size);
    }
}

#[test]
fn pressure_reclaims_other_type_and_preserves_live_indices() {
    let host = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
    let mut allocator = fixture(
        &[
            (256 * MIB, vk::MemoryPropertyFlags::DEVICE_LOCAL),
            (256 * MIB, host),
        ],
        false,
    );
    allocator.memory_types[1].heap_index = 0; // Both types consume the same physical heap.
    growable(&mut allocator, 8 * MIB, 64 * MIB);
    let anchor = allocator
        .allocate(&request(MIB, MemoryLocation::GpuOnly))
        .unwrap();
    let index = anchor.memory_block_index;
    let cached = allocator
        .allocate(&request(MIB, MemoryLocation::CpuToGpu))
        .unwrap();
    allocator.free(cached).unwrap();
    DRIVER.with(|d| d.borrow_mut().limit = Some(24 * MIB));
    let allocation = allocator
        .allocate(&request(8 * MIB, MemoryLocation::GpuOnly))
        .unwrap();
    assert_eq!(
        DRIVER.with(|d| d.borrow().calls.clone()),
        vec![(0, 8 * MIB), (1, 8 * MIB), (0, 16 * MIB), (0, 16 * MIB)]
    );
    assert_eq!(anchor.memory_block_index, index);
    assert_eq!(allocator.cached_capacity(), 0);
    assert_eq!(allocator.capacity(), 24 * MIB);
    allocator.free(allocation).unwrap();
    allocator.free(anchor).unwrap();
}

#[test]
fn smaller_block_recovers_when_preferred_growth_does_not_fit() {
    let mut allocator = fixture(&[(256 * MIB, vk::MemoryPropertyFlags::DEVICE_LOCAL)], false);
    growable(&mut allocator, 8 * MIB, 64 * MIB);
    DRIVER.with(|d| d.borrow_mut().limit = Some(12 * MIB));
    let allocation = allocator
        .allocate(&request(9 * MIB, MemoryLocation::GpuOnly))
        .unwrap();
    assert_eq!(allocator.capacity(), 9 * MIB);
    allocator.free(allocation).unwrap();
    let reused = allocator
        .allocate(&request(9 * MIB, MemoryLocation::GpuOnly))
        .unwrap();
    allocator.free(reused).unwrap();
    let larger = allocator
        .allocate(&request(11 * MIB, MemoryLocation::GpuOnly))
        .unwrap();
    assert_eq!(
        DRIVER.with(|d| d.borrow().calls.clone()),
        vec![
            (0, 16 * MIB),
            (0, 9 * MIB),
            (0, 16 * MIB),
            (0, 16 * MIB),
            (0, 11 * MIB)
        ]
    );
    assert_eq!(allocator.capacity(), 11 * MIB);
    allocator.free(larger).unwrap();
}

#[test]
fn fatal_errors_do_not_reclaim_caches_or_retry() {
    let mut allocator = fixture(&[(256 * MIB, vk::MemoryPropertyFlags::DEVICE_LOCAL)], false);
    growable(&mut allocator, 8 * MIB, 64 * MIB);
    let cached = allocator
        .allocate(&request(MIB, MemoryLocation::GpuOnly))
        .unwrap();
    allocator.free(cached).unwrap();
    DRIVER.with(|d| {
        d.borrow_mut()
            .errors
            .push_back(vk::Result::ERROR_DEVICE_LOST)
    });
    assert!(matches!(
        allocator.allocate(&request(9 * MIB, MemoryLocation::GpuOnly)),
        Err(AllocationError::Internal(_))
    ));
    assert_eq!(allocator.cached_capacity(), 8 * MIB);
    assert_eq!(DRIVER.with(|d| d.borrow().calls.len()), 2);
    assert_eq!(DRIVER.with(|d| d.borrow().freed), 0);
}

#[test]
fn allocation_count_limit_reclaims_but_never_shrinks_or_switches_types() {
    let flags = vk::MemoryPropertyFlags::DEVICE_LOCAL;
    let mut allocator = fixture(&[(256 * MIB, flags), (256 * MIB, flags)], false);
    growable(&mut allocator, 8 * MIB, 64 * MIB);
    DRIVER.with(|d| {
        d.borrow_mut()
            .errors
            .push_back(vk::Result::ERROR_TOO_MANY_OBJECTS)
    });
    assert!(allocator
        .allocate(&request(MIB, MemoryLocation::GpuOnly))
        .is_err());
    assert_eq!(DRIVER.with(|d| d.borrow().calls.len()), 1);
    let cached = allocator
        .allocate(&request(MIB, MemoryLocation::GpuOnly))
        .unwrap();
    allocator.free(cached).unwrap();
    DRIVER.with(|d| {
        d.borrow_mut()
            .errors
            .push_back(vk::Result::ERROR_TOO_MANY_OBJECTS)
    });
    let recovered = allocator
        .allocate(&request(9 * MIB, MemoryLocation::GpuOnly))
        .unwrap();
    assert_eq!(DRIVER.with(|d| d.borrow().calls.len()), 4);
    assert_eq!(DRIVER.with(|d| d.borrow().freed), 1);
    allocator.free(recovered).unwrap();
}

#[test]
fn host_oom_and_mapping_pressure_recover_and_clean_up() {
    for error in [
        vk::Result::ERROR_OUT_OF_HOST_MEMORY,
        vk::Result::ERROR_MEMORY_MAP_FAILED,
    ] {
        let host = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
        let mut allocator = fixture(&[(256 * MIB, host)], false);
        growable(&mut allocator, 8 * MIB, 64 * MIB);
        allocator.memory_types[0].mappable = true;
        DRIVER.with(|d| d.borrow_mut().map_errors.push_back(error));
        let allocation = allocator
            .allocate(&request(MIB, MemoryLocation::CpuToGpu))
            .unwrap();
        assert_eq!(
            DRIVER.with(|d| d.borrow().calls.clone()),
            vec![(0, 8 * MIB), (0, 4 * MIB)]
        );
        assert_eq!(DRIVER.with(|d| d.borrow().freed), 1);
        assert!(allocation.mapped_ptr().is_some());
        allocator.free(allocation).unwrap();
        drop(allocator);
        assert_eq!(DRIVER.with(|d| d.borrow().freed), 2);
        assert_eq!(DRIVER.with(|d| d.borrow().unmapped), 1);
        assert!(DRIVER.with(|d| d.borrow().live.is_empty()));
    }
}

#[test]
fn fatal_mapping_and_null_pointer_roll_back_without_retry() {
    for null_map in [false, true] {
        let host = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
        let mut allocator = fixture(&[(256 * MIB, host)], false);
        allocator.memory_types[0].mappable = true;
        DRIVER.with(|d| {
            let mut d = d.borrow_mut();
            d.null_map = null_map;
            if !null_map {
                d.map_errors.push_back(vk::Result::ERROR_DEVICE_LOST);
            }
        });
        assert!(matches!(
            allocator.allocate(&request(MIB, MemoryLocation::CpuToGpu)),
            Err(AllocationError::Internal(_))
        ));
        assert_eq!(allocator.capacity(), 0);
        assert!(allocator.memory_types[0].memory_blocks.is_empty());
        assert_eq!(DRIVER.with(|d| d.borrow().freed), 1);
        assert_eq!(DRIVER.with(|d| d.borrow().unmapped), usize::from(null_map));
    }
}

#[test]
fn cached_blocks_do_not_advance_growth() {
    let mut allocator = fixture(&[(256 * MIB, vk::MemoryPropertyFlags::DEVICE_LOCAL)], false);
    growable(&mut allocator, 4 * MIB, 64 * MIB);
    let first = allocator
        .allocate(&request(3 * MIB, MemoryLocation::GpuOnly))
        .unwrap();
    let second = allocator
        .allocate(&request(5 * MIB, MemoryLocation::GpuOnly))
        .unwrap();
    allocator.free(first).unwrap();
    let third = allocator
        .allocate(&request(5 * MIB, MemoryLocation::GpuOnly))
        .unwrap();
    assert_eq!(
        DRIVER.with(|d| d.borrow().calls.clone()),
        vec![(0, 4 * MIB), (0, 8 * MIB), (0, 8 * MIB)]
    );
    allocator.free(second).unwrap();
    allocator.free(third).unwrap();
}

#[test]
fn trim_to_bounds_aggregate_cache_and_ignores_historical_slots() {
    let flags = vk::MemoryPropertyFlags::DEVICE_LOCAL;
    let mut allocator = fixture(
        &[(256 * MIB, flags), (256 * MIB, flags), (256 * MIB, flags)],
        false,
    );
    growable(&mut allocator, 4 * MIB, 64 * MIB);
    for (index, size) in [(0, MIB), (1, 5 * MIB), (2, 7 * MIB)] {
        let mut desc = request(size, MemoryLocation::GpuOnly);
        desc.requirements.memory_type_bits = 1 << index;
        let allocation = allocator.allocate(&desc).unwrap();
        allocator.free(allocation).unwrap();
        allocator.memory_types[index]
            .memory_blocks
            .resize_with(100_000, || None);
    }
    assert_eq!(allocator.cached_capacity(), 20 * MIB);
    assert_eq!(allocator.trim_to(12 * MIB), 8 * MIB);
    assert_eq!(allocator.trim_to(12 * MIB), 0);
    assert_eq!(allocator.trim_to(5 * MIB), 8 * MIB);
    assert_eq!(allocator.trim(), 4 * MIB);
    assert_eq!(allocator.trim(), 0);
    assert_eq!(DRIVER.with(|d| d.borrow().freed), 3);
}

#[test]
fn first_suballocation_failure_is_atomic() {
    let mut allocator = fixture(&[(256 * MIB, vk::MemoryPropertyFlags::DEVICE_LOCAL)], false);
    let desc = request(8 * MIB, MemoryLocation::GpuOnly);
    let result = allocator.allocate_type(0, &desc, &Arc::new(Backtrace::disabled()), Some(MIB));
    assert!(result.is_err());
    let error = result.err().unwrap();
    assert!(!error.is_memory_pressure());
    assert!(matches!(error.error, AllocationError::Internal(_)));
    assert!(allocator.memory_types[0].memory_blocks.is_empty());
    assert_eq!(allocator.memory_types[0].active_general_blocks, 0);
    assert_eq!(allocator.cached_capacity(), 0);
    assert_eq!(DRIVER.with(|d| d.borrow().freed), 1);
}

#[test]
fn compatible_cache_is_reused_before_global_reclamation() {
    let host = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
    let mut allocator = fixture(
        &[
            (256 * MIB, host),
            (256 * MIB, host | vk::MemoryPropertyFlags::DEVICE_LOCAL),
        ],
        false,
    );
    growable(&mut allocator, 8 * MIB, 64 * MIB);
    let mut desc = request(MIB, MemoryLocation::CpuToGpu);
    desc.requirements.memory_type_bits = 1;
    let cached = allocator.allocate(&desc).unwrap();
    allocator.free(cached).unwrap();
    DRIVER.with(|d| {
        d.borrow_mut()
            .errors
            .push_back(vk::Result::ERROR_OUT_OF_DEVICE_MEMORY)
    });
    desc.requirements.memory_type_bits = 3;
    let allocation = allocator.allocate(&desc).unwrap();
    assert_eq!(allocation.memory_type_index, 0);
    assert_eq!(DRIVER.with(|d| d.borrow().calls.len()), 2);
    assert_eq!(DRIVER.with(|d| d.borrow().freed), 0);
    allocator.free(allocation).unwrap();
}

#[test]
fn allocation_count_guard_runs_before_driver_and_reuses_existing_blocks() {
    let flags = vk::MemoryPropertyFlags::DEVICE_LOCAL;
    let mut allocator = fixture(&[(256 * MIB, flags)], false);
    growable(&mut allocator, 8 * MIB, 64 * MIB);
    allocator.max_memory_allocation_count = 1;
    let first = allocator
        .allocate(&request(MIB, MemoryLocation::GpuOnly))
        .unwrap();
    let second = allocator
        .allocate(&request(MIB, MemoryLocation::GpuOnly))
        .unwrap();
    assert!(allocator
        .allocate(&request(9 * MIB, MemoryLocation::GpuOnly))
        .is_err());
    assert_eq!(DRIVER.with(|d| d.borrow().calls.len()), 1);
    allocator.free(first).unwrap();
    allocator.free(second).unwrap();
    let larger = allocator
        .allocate(&request(9 * MIB, MemoryLocation::GpuOnly))
        .unwrap();
    assert_eq!(DRIVER.with(|d| d.borrow().calls.len()), 2);
    assert_eq!(allocator.memory_types[0].active_blocks, 1);
    allocator.free(larger).unwrap();
    allocator.trim();
    assert_eq!(allocator.memory_types[0].active_blocks, 0);
}

#[test]
fn native_host_oom_recovers_and_failed_mapping_preserves_first_cause() {
    let mut allocator = fixture(&[(256 * MIB, vk::MemoryPropertyFlags::DEVICE_LOCAL)], false);
    growable(&mut allocator, 8 * MIB, 64 * MIB);
    DRIVER.with(|d| {
        d.borrow_mut()
            .errors
            .push_back(vk::Result::ERROR_OUT_OF_HOST_MEMORY)
    });
    let allocation = allocator
        .allocate(&request(MIB, MemoryLocation::GpuOnly))
        .unwrap();
    assert_eq!(
        DRIVER.with(|d| d.borrow().calls.clone()),
        vec![(0, 8 * MIB), (0, 4 * MIB)]
    );
    allocator.free(allocation).unwrap();
    drop(allocator);
    let host = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
    let mut allocator = fixture(&[(256 * MIB, host)], false);
    growable(&mut allocator, 8 * MIB, 64 * MIB);
    allocator.memory_types[0].mappable = true;
    DRIVER.with(|d| {
        let mut d = d.borrow_mut();
        d.map_errors.push_back(vk::Result::ERROR_MEMORY_MAP_FAILED);
        d.map_fail = Some(vk::Result::ERROR_OUT_OF_DEVICE_MEMORY);
    });
    assert!(matches!(
        allocator.allocate(&request(MIB, MemoryLocation::CpuToGpu)),
        Err(AllocationError::FailedToMap(_))
    ));
    assert_eq!(
        DRIVER.with(|d| d.borrow().freed),
        DRIVER.with(|d| d.borrow().calls.len())
    );
    assert_eq!(allocator.capacity(), 0);
    assert_eq!(allocator.memory_types[0].active_blocks, 0);
}

#[test]
fn aggregate_empty_cache_never_exceeds_the_physical_heap() {
    let flags = vk::MemoryPropertyFlags::DEVICE_LOCAL;
    let mut allocator = fixture(
        &[(8 * MIB, flags), (8 * MIB, flags), (8 * MIB, flags)],
        false,
    );
    growable(&mut allocator, 4 * MIB, 8 * MIB);
    for ty in &mut allocator.memory_types {
        ty.heap_index = 0;
    }
    let mut allocations = Vec::new();
    for index in 0..3 {
        let mut desc = request(MIB, MemoryLocation::GpuOnly);
        desc.requirements.memory_type_bits = 1 << index;
        allocations.push(allocator.allocate(&desc).unwrap());
    }
    // The fake driver permits overallocation, like some real implementations.
    assert_eq!(allocator.capacity(), 12 * MIB);
    for allocation in allocations {
        allocator.free(allocation).unwrap();
    }
    assert_eq!(allocator.cached_capacity(), 8 * MIB);
    assert_eq!(DRIVER.with(|d| d.borrow().freed), 1);
}

#[test]
fn dedicated_recovery_preserves_resource_and_device_address_chain() {
    for scheme in [
        AllocationScheme::DedicatedBuffer(vk::Buffer::from_raw(37)),
        AllocationScheme::DedicatedImage(vk::Image::from_raw(44)),
    ] {
        let mut allocator = fixture(&[(256 * MIB, vk::MemoryPropertyFlags::DEVICE_LOCAL)], false);
        growable(&mut allocator, 8 * MIB, 64 * MIB);
        allocator.memory_types[0].buffer_device_address = true;
        let cached = allocator
            .allocate(&request(MIB, MemoryLocation::GpuOnly))
            .unwrap();
        allocator.free(cached).unwrap();
        DRIVER.with(|d| {
            d.borrow_mut()
                .errors
                .push_back(vk::Result::ERROR_OUT_OF_DEVICE_MEMORY)
        });
        let mut desc = request(3 * MIB, MemoryLocation::GpuOnly);
        desc.allocation_scheme = scheme;
        let allocation = allocator.allocate(&desc).unwrap();
        let expected = match scheme {
            AllocationScheme::DedicatedBuffer(buffer) => (buffer, vk::Image::null()),
            AllocationScheme::DedicatedImage(image) => (vk::Buffer::null(), image),
            AllocationScheme::GpuAllocatorManaged => unreachable!(),
        };
        DRIVER.with(|d| {
            let d = d.borrow();
            assert_eq!(d.calls, vec![(0, 8 * MIB), (0, 3 * MIB), (0, 3 * MIB)]);
            assert_eq!(d.dedicated, vec![None, Some(expected), Some(expected)]);
            assert!(d
                .allocation_flags
                .iter()
                .all(|flags| flags.contains(vk::MemoryAllocateFlags::DEVICE_ADDRESS)));
        });
        assert!(allocation.dedicated_allocation);
        allocator.free(allocation).unwrap();
        assert_eq!(allocator.cached_capacity(), 0);
        assert_eq!(allocator.memory_types[0].active_blocks, 0);
        assert_eq!(DRIVER.with(|d| d.borrow().freed), 2);
    }
}

#[test]
fn pressure_backoff_keeps_useful_capacity_before_the_allocation_count_limit() {
    let mut allocator = fixture(&[(256 * MIB, vk::MemoryPropertyFlags::DEVICE_LOCAL)], false);
    growable(&mut allocator, 64 * MIB, 64 * MIB);
    allocator.max_memory_allocation_count = 2;
    DRIVER.with(|d| d.borrow_mut().limit = Some(48 * MIB));
    let mut live = Vec::new();
    for _ in 0..48 {
        live.push(
            allocator
                .allocate(&request(MIB, MemoryLocation::GpuOnly))
                .unwrap(),
        );
    }
    assert_eq!(allocator.capacity(), 48 * MIB);
    assert_eq!(allocator.memory_types[0].active_blocks, 2);
    assert_eq!(
        DRIVER.with(|d| d.borrow().calls.clone()),
        vec![
            (0, 64 * MIB),
            (0, 32 * MIB),
            (0, 64 * MIB),
            (0, 32 * MIB),
            (0, 16 * MIB)
        ]
    );
    for allocation in live {
        allocator.free(allocation).unwrap();
    }
    allocator.trim();
    assert!(DRIVER.with(|d| d.borrow().live.is_empty()));
}

#[test]
fn fallback_excludes_implicitly_enabled_device_coherent_memory() {
    let host = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
    let mut a = fixture(
        &[
            (16 * MIB, host),
            (
                16 * MIB,
                host | vk::MemoryPropertyFlags::DEVICE_COHERENT_AMD,
            ),
        ],
        true,
    );
    assert!(a.allocate(&request(MIB, MemoryLocation::CpuToGpu)).is_err());
    assert!(DRIVER.with(|d| d.borrow().calls.iter().all(|&(index, _)| index == 0)));
}
