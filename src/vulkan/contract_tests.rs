#![allow(clippy::unwrap_used)]
use super::test_support::*;
use super::*;
const MIB: u64 = 1024 * 1024;
const HOST: vk::MemoryPropertyFlags = vk::MemoryPropertyFlags::from_raw(
    vk::MemoryPropertyFlags::HOST_VISIBLE.as_raw()
        | vk::MemoryPropertyFlags::HOST_COHERENT.as_raw(),
);

#[test]
fn fallback_does_not_implicitly_enable_device_coherent_memory() {
    let mut a = fixture(
        &[
            (16 * MIB, HOST),
            (
                16 * MIB,
                HOST | vk::MemoryPropertyFlags::DEVICE_COHERENT_AMD,
            ),
        ],
        true,
    );
    assert!(a.allocate(&request(MIB, MemoryLocation::CpuToGpu)).is_err());
    assert_eq!(DRIVER.with(|d| d.borrow().calls.len()), 1);
}

#[test]
fn small_and_non_quantized_heaps_bound_every_native_block() {
    for capacity in [1, 2 * MIB, 4 * MIB - 1, 7 * MIB + 123] {
        let mut a = fixture(&[(capacity, vk::MemoryPropertyFlags::DEVICE_LOCAL)], false);
        let allocation = a.allocate(&request(1, MemoryLocation::GpuOnly)).unwrap();
        assert_eq!(
            DRIVER.with(|d| d.borrow().calls.clone()),
            vec![(0, capacity)]
        );
        a.free(allocation).unwrap();
    }
}
#[test]
fn oversized_fallback_heap_is_skipped_before_native_call() {
    let mut a = fixture(
        &[
            (2 * MIB, HOST),
            (16 * MIB, HOST | vk::MemoryPropertyFlags::DEVICE_LOCAL),
        ],
        true,
    );
    assert!(matches!(
        a.allocate(&request(4 * MIB, MemoryLocation::CpuToGpu)),
        Err(AllocationError::OutOfMemory)
    ));
    assert_eq!(
        DRIVER.with(|d| d.borrow().calls.clone()),
        vec![(1, 8 * MIB)]
    );
}
#[test]
fn a_memory_type_is_never_retried_as_its_own_fallback() {
    let mut a = fixture(&[(16 * MIB, HOST)], true);
    assert!(a.allocate(&request(MIB, MemoryLocation::CpuToGpu)).is_err());
    assert_eq!(DRIVER.with(|d| d.borrow().calls.len()), 1);
}
#[test]
fn fatal_native_allocation_error_is_not_hidden_by_fallback() {
    let mut a = fixture(
        &[
            (16 * MIB, HOST),
            (16 * MIB, HOST | vk::MemoryPropertyFlags::DEVICE_LOCAL),
        ],
        false,
    );
    DRIVER.with(|d| {
        d.borrow_mut()
            .errors
            .push_back(vk::Result::ERROR_DEVICE_LOST)
    });
    let error = a
        .allocate(&request(MIB, MemoryLocation::CpuToGpu))
        .unwrap_err();
    assert!(
        matches!(error, AllocationError::Internal(ref message) if message.contains("ERROR_DEVICE_LOST"))
    );
    assert_eq!(DRIVER.with(|d| d.borrow().calls.len()), 1);
}
#[test]
fn host_memory_exhaustion_can_use_another_compatible_type() {
    let mut a = fixture(
        &[
            (16 * MIB, HOST),
            (16 * MIB, HOST | vk::MemoryPropertyFlags::DEVICE_LOCAL),
        ],
        false,
    );
    DRIVER.with(|d| {
        d.borrow_mut()
            .errors
            .push_back(vk::Result::ERROR_OUT_OF_HOST_MEMORY)
    });
    let allocation = a.allocate(&request(MIB, MemoryLocation::CpuToGpu)).unwrap();
    assert_eq!(allocation.memory_type_index, 0);
    assert_eq!(
        DRIVER.with(|d| d.borrow().calls.iter().map(|r| r.0).collect::<Vec<_>>()),
        vec![1, 0]
    );
    a.free(allocation).unwrap();
}
#[test]
fn allocation_count_limit_includes_personal_blocks_and_recovers_after_free() {
    let mut a = fixture_with_limit(
        &[(32 * MIB, vk::MemoryPropertyFlags::DEVICE_LOCAL)],
        false,
        1,
    );
    let first = a
        .allocate(&request(9 * MIB, MemoryLocation::GpuOnly))
        .unwrap();
    assert!(a
        .allocate(&request(9 * MIB, MemoryLocation::GpuOnly))
        .is_err());
    assert_eq!(DRIVER.with(|d| d.borrow().calls.len()), 1);
    a.free(first).unwrap();
    let next = a
        .allocate(&request(9 * MIB, MemoryLocation::GpuOnly))
        .unwrap();
    a.free(next).unwrap();
    assert_eq!(DRIVER.with(|d| d.borrow().live.len()), 0);
}
#[test]
fn count_limit_still_allows_existing_space_in_a_later_type() {
    let mut a = fixture_with_limit(
        &[
            (8 * MIB, vk::MemoryPropertyFlags::DEVICE_LOCAL),
            (8 * MIB, HOST),
        ],
        false,
        2,
    );
    let full = a
        .allocate(&request(8 * MIB, MemoryLocation::GpuOnly))
        .unwrap();
    let anchor = a.allocate(&request(MIB, MemoryLocation::CpuToGpu)).unwrap();
    let allocation = a.allocate(&request(MIB, MemoryLocation::Unknown)).unwrap();
    assert_eq!(allocation.memory_type_index, 1);
    assert_eq!(DRIVER.with(|d| d.borrow().calls.len()), 2);
    a.free(allocation).unwrap();
    a.free(anchor).unwrap();
    a.free(full).unwrap();
}
#[test]
fn failed_mapping_keeps_cleanup_and_stops_on_fatal_causes() {
    let mut a = fixture(&[(8 * MIB, HOST)], false);
    DRIVER.with(|d| d.borrow_mut().map_fail = Some(vk::Result::ERROR_DEVICE_LOST));
    assert!(matches!(
        a.allocate(&request(MIB, MemoryLocation::CpuToGpu)),
        Err(AllocationError::Internal(_))
    ));
    assert_eq!(DRIVER.with(|d| d.borrow().freed), 1);
    assert!(DRIVER.with(|d| d.borrow().live.is_empty()));
    assert_eq!(a.capacity(), 0);
}
#[test]
fn anomalous_null_mapping_unmaps_and_releases_unpublished_memory() {
    let mut a = fixture(&[(8 * MIB, HOST)], false);
    DRIVER.with(|d| d.borrow_mut().null_map = true);
    assert!(matches!(
        a.allocate(&request(MIB, MemoryLocation::CpuToGpu)),
        Err(AllocationError::Internal(_))
    ));
    assert_eq!(DRIVER.with(|d| d.borrow().unmapped), 1);
    assert_eq!(DRIVER.with(|d| d.borrow().freed), 1);
}
