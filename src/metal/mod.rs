#[cfg(feature = "std")]
use alloc::sync::Arc;
use alloc::{boxed::Box, vec::Vec};
#[cfg(feature = "std")]
use std::backtrace::Backtrace;

use log::debug;
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_foundation::{ns_string, NSString};
#[cfg(doc)]
use objc2_metal::{MTLAllocation, MTLResource};
use objc2_metal::{
    MTLCPUCacheMode, MTLDevice, MTLHeap, MTLHeapDescriptor, MTLHeapType, MTLResidencySet,
    MTLResourceOptions, MTLStorageMode, MTLTextureDescriptor,
};

#[cfg(feature = "visualizer")]
mod visualizer;
#[cfg(feature = "visualizer")]
pub use visualizer::AllocatorVisualizer;

use crate::{
    allocator::{
        AllocationType, AllocatorReport, DedicatedBlockAllocator, FreeListAllocator,
        MemoryBlockReport, SubAllocator,
    },
    AllocationError, AllocationFailure, AllocationResult, AllocationSizes, AllocatorDebugSettings,
    MemoryBlockSizes, MemoryLocation, Result,
};

/// Shared placement heaps use the CPU-visible range; Private heaps use the GPU-only range.
fn uses_host_block_sizes(storage_mode: MTLStorageMode) -> bool {
    storage_mode == MTLStorageMode::Shared
}

fn memory_location_to_metal(location: MemoryLocation) -> MTLResourceOptions {
    match location {
        MemoryLocation::GpuOnly => MTLResourceOptions::StorageModePrivate,
        MemoryLocation::CpuToGpu | MemoryLocation::GpuToCpu | MemoryLocation::Unknown => {
            MTLResourceOptions::StorageModeShared
        }
    }
}

#[derive(Debug)]
pub struct Allocation {
    chunk_id: Option<core::num::NonZeroU64>,
    offset: u64,
    size: u64,
    memory_block_index: usize,
    memory_type_index: usize,
    heap: Retained<ProtocolObject<dyn MTLHeap>>,
    name: Option<Box<str>>,
}

impl Allocation {
    /// Returns the [`MTLHeap`] object that is backing this allocation.
    ///
    /// This heap object can be shared with multiple other allocations and shouldn't be allocated from
    /// without this library, because that will lead to undefined behavior.
    ///
    /// # Safety
    /// When allocating new buffers, textures, or other resources on this [`MTLHeap`], be sure to
    /// pass [`Self::offset()`] and not exceed [`Self::size()`] to not allocate new resources on top
    /// of existing [`Allocation`]s.
    ///
    /// Also, this [`Allocation`] must not be [`Allocator::free()`]d while such a created resource
    /// on this [`MTLHeap`] is still live.
    pub unsafe fn heap(&self) -> &ProtocolObject<dyn MTLHeap> {
        &self.heap
    }

    /// Returns the size of the allocation
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Returns the offset of the allocation on the [`MTLHeap`].
    ///
    /// Since all [`Allocation`]s are suballocated within a [`MTLHeap`], this offset always needs to
    /// be supplied.  See the safety documentation on [`Self::heap()`].
    pub fn offset(&self) -> u64 {
        self.offset
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    fn is_null(&self) -> bool {
        self.chunk_id.is_none()
    }
}

#[derive(Clone, Debug)]
pub struct AllocationCreateDesc<'a> {
    /// Name of the allocation, for tracking and debugging purposes
    pub name: &'a str,
    /// Location where the memory allocation should be stored
    pub location: MemoryLocation,
    pub size: u64,
    pub alignment: u64,
}

impl<'a> AllocationCreateDesc<'a> {
    pub fn buffer(
        device: &ProtocolObject<dyn MTLDevice>,
        name: &'a str,
        length: u64,
        location: MemoryLocation,
    ) -> Self {
        let size_and_align = device.heapBufferSizeAndAlignWithLength_options(
            length as usize,
            memory_location_to_metal(location),
        );
        Self {
            name,
            location,
            size: size_and_align.size as u64,
            alignment: size_and_align.align as u64,
        }
    }

    pub fn texture(
        device: &ProtocolObject<dyn MTLDevice>,
        name: &'a str,
        desc: &MTLTextureDescriptor,
    ) -> Self {
        let size_and_align = device.heapTextureSizeAndAlignWithDescriptor(desc);
        Self {
            name,
            location: match desc.storageMode() {
                MTLStorageMode::Shared | MTLStorageMode::Managed | MTLStorageMode::Memoryless => {
                    MemoryLocation::Unknown
                }
                MTLStorageMode::Private => MemoryLocation::GpuOnly,
                MTLStorageMode(mode /* @ 4.. */) => todo!("Unknown storage mode {mode}"),
            },
            size: size_and_align.size as u64,
            alignment: size_and_align.align as u64,
        }
    }

    pub fn acceleration_structure_with_size(
        device: &ProtocolObject<dyn MTLDevice>,
        name: &'a str,
        size: u64, // TODO: usize
        location: MemoryLocation,
    ) -> Self {
        // TODO: See if we can mark this function as safe, after checking what happens if size is too large?
        // What other preconditions need to be upheld?
        let size_and_align =
            unsafe { device.heapAccelerationStructureSizeAndAlignWithSize(size as usize) };
        Self {
            name,
            location,
            size: size_and_align.size as u64,
            alignment: size_and_align.align as u64,
        }
    }
}

pub struct Allocator {
    device: Retained<ProtocolObject<dyn MTLDevice>>,
    global_residency_set: Option<Retained<ProtocolObject<dyn MTLResidencySet>>>,
    debug_settings: AllocatorDebugSettings,
    memory_types: Vec<MemoryType>,
}

impl core::fmt::Debug for Allocator {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.generate_report().fmt(f)
    }
}

#[derive(Debug)]
pub struct AllocatorCreateDesc {
    pub device: Retained<ProtocolObject<dyn MTLDevice>>,
    pub debug_settings: AllocatorDebugSettings,
    pub allocation_sizes: AllocationSizes,
    /// Whether to create a [`MTLResidencySet`] containing all live heaps, that can be retrieved via
    /// [`Allocator::residency_set()`].  Only supported on `MacOS 15.0+` / `iOS 18.0+`.
    pub create_residency_set: bool,
}

#[derive(Debug)]
pub struct CommittedAllocationStatistics {
    pub num_allocations: usize,
    pub total_size: u64,
}

#[derive(Debug)]
struct MemoryBlock {
    heap: Retained<ProtocolObject<dyn MTLHeap>>,
    size: u64,
    sub_allocator: Box<dyn SubAllocator>,
}

impl MemoryBlock {
    fn new(
        device: &ProtocolObject<dyn MTLDevice>,
        size: u64,
        heap_descriptor: &MTLHeapDescriptor,
        dedicated: bool,
        memory_location: MemoryLocation,
    ) -> AllocationResult<Self> {
        heap_descriptor.setSize(size as usize);

        let heap = device
            .newHeapWithDescriptor(heap_descriptor)
            .ok_or_else(|| {
                AllocationFailure::memory(AllocationError::Internal(
                    "No MTLHeap could be created with the given size".into(),
                ))
            })?;

        heap.setLabel(Some(&NSString::from_str(&format!(
            "MemoryBlock {memory_location:?}"
        ))));

        let sub_allocator: Box<dyn SubAllocator> = if dedicated {
            Box::new(DedicatedBlockAllocator::new(size))
        } else {
            Box::new(FreeListAllocator::new(size))
        };

        Ok(Self {
            heap,
            size,
            sub_allocator,
        })
    }
}

#[derive(Debug)]
struct MemoryType {
    global_residency_set: Option<Retained<ProtocolObject<dyn MTLResidencySet>>>,
    memory_blocks: Vec<Option<MemoryBlock>>,
    _committed_allocations: CommittedAllocationStatistics,
    memory_location: MemoryLocation,
    heap_properties: Retained<MTLHeapDescriptor>,
    memory_type_index: usize,
    active_general_blocks: usize,
    empty_block_index: Option<usize>,
    pending_residency_removals: bool,
    block_sizes: MemoryBlockSizes,
}

impl MemoryType {
    fn block_size(&self, desc: &AllocationCreateDesc<'_>) -> u64 {
        let size = desc.size;
        if size > self.block_sizes.dedicated_threshold() {
            return size;
        }
        let occupied = self.active_general_blocks - usize::from(self.empty_block_index.is_some());
        self.block_sizes.shared_block_size(occupied, size)
    }

    fn cached_size(&self) -> u64 {
        self.empty_block_index
            .and_then(|i| self.memory_blocks.get(i))
            .and_then(Option::as_ref)
            .filter(|block| block.sub_allocator.is_empty())
            .map_or(0, |block| block.size)
    }

    fn release_cached(&mut self) -> u64 {
        let Some(index) = self.empty_block_index.take() else {
            return 0;
        };
        let Some(slot) = self.memory_blocks.get_mut(index) else {
            return 0;
        };
        if !slot.as_ref().is_some_and(|block| {
            block.sub_allocator.is_empty() && block.sub_allocator.supports_general_allocations()
        }) {
            return 0;
        }
        if let Some(block) = slot.take() {
            let bytes = block.size;
            self.active_general_blocks -= 1;
            if let Some(rs) = &self.global_residency_set {
                rs.removeAllocation(block.heap.as_ref());
                self.pending_residency_removals = true;
            }
            drop(block);
            bytes
        } else {
            0
        }
    }

    fn allocate(
        &mut self,
        device: &ProtocolObject<dyn MTLDevice>,
        desc: &AllocationCreateDesc<'_>,

        #[cfg(feature = "std")] backtrace: Arc<Backtrace>,
        new_block_size: Option<u64>,
    ) -> AllocationResult<Allocation> {
        let size = desc.size;
        let alignment = desc.alignment;

        let allocation_type = AllocationType::Linear;
        let dedicated_allocation = false;
        let dedicated = dedicated_allocation || size > self.block_sizes.dedicated_threshold();
        let mut vacant = None;
        if !dedicated {
            for (index, slot) in self.memory_blocks.iter_mut().enumerate().rev() {
                let Some(block) = slot else {
                    if vacant.is_none() {
                        vacant = Some(index);
                    }
                    continue;
                };
                match block.sub_allocator.allocate(
                    size,
                    alignment,
                    allocation_type,
                    1,
                    desc.name,
                    #[cfg(feature = "std")]
                    backtrace.clone(),
                ) {
                    Ok((offset, chunk_id)) => {
                        if self.empty_block_index == Some(index) {
                            self.empty_block_index = None;
                        }

                        return Ok(Allocation {
                            chunk_id: Some(chunk_id),
                            size,
                            offset,
                            memory_block_index: index,
                            memory_type_index: self.memory_type_index,
                            name: Some(desc.name.into()),
                            heap: block.heap.clone(),
                        });
                    }
                    Err(AllocationError::OutOfMemory) => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
        let Some(block_size) = new_block_size else {
            return Err(AllocationError::OutOfMemory.into());
        };
        let mut block = MemoryBlock::new(
            device,
            if dedicated { size } else { block_size },
            &self.heap_properties,
            dedicated,
            self.memory_location,
        )?;
        let (offset, chunk_id) = match block.sub_allocator.allocate(
            size,
            alignment,
            allocation_type,
            1,
            desc.name,
            #[cfg(feature = "std")]
            backtrace,
        ) {
            Ok(value) => value,
            Err(error) => {
                drop(block);
                return Err(AllocationFailure::fatal(error));
            }
        };
        if let Some(rs) = &self.global_residency_set {
            rs.addAllocation(block.heap.as_ref());
        }
        let index = if dedicated {
            self.memory_blocks.iter().position(Option::is_none)
        } else {
            vacant
        };
        let index = match index {
            Some(index) => {
                self.memory_blocks[index] = Some(block);
                index
            }
            None => {
                self.memory_blocks.push(Some(block));
                self.memory_blocks.len() - 1
            }
        };
        if !dedicated {
            self.active_general_blocks += 1;
        }
        let block = self.memory_blocks[index]
            .as_ref()
            .ok_or_else(|| AllocationError::Internal("Published memory block is missing".into()))?;

        Ok(Allocation {
            chunk_id: Some(chunk_id),
            size,
            offset,
            memory_block_index: index,
            memory_type_index: self.memory_type_index,
            name: Some(desc.name.into()),
            heap: block.heap.clone(),
        })
    }

    fn free(&mut self, allocation: &Allocation) -> Result<()> {
        let block_idx = allocation.memory_block_index;

        let mem_block = self.memory_blocks[block_idx]
            .as_mut()
            .ok_or_else(|| AllocationError::Internal("Memory block must be Some.".into()))?;

        mem_block.sub_allocator.free(allocation.chunk_id)?;

        if !mem_block.sub_allocator.is_empty() {
            return Ok(());
        }
        let mut block_idx = block_idx;
        let general = mem_block.sub_allocator.supports_general_allocations();
        let block_size = mem_block.size;
        if general {
            let cached_size = self
                .empty_block_index
                .and_then(|i| self.memory_blocks[i].as_ref())
                .map(|block| block.size);
            if self
                .block_sizes
                .should_cache_empty_block(block_size, cached_size)
            {
                let previous = self.empty_block_index.replace(block_idx);
                if let Some(previous) = previous {
                    block_idx = previous;
                } else {
                    return Ok(());
                }
            }
        }
        {
            let block = self.memory_blocks[block_idx]
                .take()
                .ok_or_else(|| AllocationError::Internal("Memory block must be Some.".into()))?;

            if block.sub_allocator.supports_general_allocations() {
                self.active_general_blocks -= 1;
            }

            if let Some(rs) = &self.global_residency_set {
                rs.removeAllocation(block.heap.as_ref());
                self.pending_residency_removals = true;
            }

            // Note that `block` will be destroyed on `drop` here
        }

        Ok(())
    }
}

impl Allocator {
    pub fn new(desc: &AllocatorCreateDesc) -> Result<Self> {
        let heap_types = [
            (MemoryLocation::GpuOnly, {
                let heap_desc = MTLHeapDescriptor::new();
                heap_desc.setCpuCacheMode(MTLCPUCacheMode::DefaultCache);
                heap_desc.setStorageMode(MTLStorageMode::Private);
                heap_desc.setType(MTLHeapType::Placement);
                heap_desc
            }),
            (MemoryLocation::CpuToGpu, {
                let heap_desc = MTLHeapDescriptor::new();
                heap_desc.setCpuCacheMode(MTLCPUCacheMode::WriteCombined);
                heap_desc.setStorageMode(MTLStorageMode::Shared);
                heap_desc.setType(MTLHeapType::Placement);
                heap_desc
            }),
            (MemoryLocation::GpuToCpu, {
                let heap_desc = MTLHeapDescriptor::new();
                heap_desc.setCpuCacheMode(MTLCPUCacheMode::DefaultCache);
                heap_desc.setStorageMode(MTLStorageMode::Shared);
                heap_desc.setType(MTLHeapType::Placement);
                heap_desc
            }),
        ];

        let global_residency_set = if desc.create_residency_set {
            let rs_desc = objc2_metal::MTLResidencySetDescriptor::new();
            rs_desc.setLabel(Some(ns_string!("gpu-allocator global residency set")));
            Some(
                desc.device
                    .newResidencySetWithDescriptor_error(&rs_desc)
                    .expect("Failed to create MTLResidencySet.  Unsupported MacOS/iOS version?"),
            )
        } else {
            None
        };

        let memory_types = heap_types
            .into_iter()
            .enumerate()
            .map(|(i, (memory_location, heap_descriptor))| MemoryType {
                block_sizes: desc
                    .allocation_sizes
                    .block_sizes_for(uses_host_block_sizes(heap_descriptor.storageMode())),
                global_residency_set: global_residency_set.clone(),
                memory_blocks: vec![],
                _committed_allocations: CommittedAllocationStatistics {
                    num_allocations: 0,
                    total_size: 0,
                },
                memory_location,
                heap_properties: heap_descriptor,
                memory_type_index: i,
                active_general_blocks: 0,
                empty_block_index: None,
                pending_residency_removals: false,
            })
            .collect();

        Ok(Self {
            device: desc.device.clone(),
            debug_settings: desc.debug_settings,
            memory_types,
            global_residency_set,
        })
    }

    fn allocate_type(
        &mut self,
        index: usize,
        desc: &AllocationCreateDesc<'_>,
        #[cfg(feature = "std")] backtrace: &Arc<Backtrace>,
        block_size: Option<u64>,
    ) -> AllocationResult<Allocation> {
        self.memory_types[index].allocate(
            &self.device,
            desc,
            #[cfg(feature = "std")]
            backtrace.clone(),
            block_size,
        )
    }

    pub fn allocate(&mut self, desc: &AllocationCreateDesc<'_>) -> Result<Allocation> {
        let size = desc.size;
        let alignment = desc.alignment;

        #[cfg(feature = "std")]
        let backtrace = Arc::new(if self.debug_settings.store_stack_traces {
            Backtrace::force_capture()
        } else {
            Backtrace::disabled()
        });

        if self.debug_settings.log_allocations {
            debug!(
                "Allocating `{}` of {} bytes with an alignment of {}.",
                desc.name, size, alignment
            );
            #[cfg(feature = "std")]
            if self.debug_settings.log_stack_traces {
                let backtrace = Backtrace::force_capture();
                debug!("Allocation stack trace: {backtrace}");
            }
        }

        if size == 0 || !alignment.is_power_of_two() {
            return Err(AllocationError::InvalidAllocationCreateDesc);
        }

        let index = self
            .memory_types
            .iter()
            .position(|ty| {
                desc.location == MemoryLocation::Unknown || desc.location == ty.memory_location
            })
            .ok_or(AllocationError::NoCompatibleMemoryTypeFound)?;
        let mut attempted_size = self.memory_types[index].block_size(desc);
        let first_error = match self.allocate_type(
            index,
            desc,
            #[cfg(feature = "std")]
            &backtrace,
            Some(attempted_size),
        ) {
            Ok(allocation) => return Ok(allocation),
            Err(error) if error.is_memory_pressure() => error,
            Err(error) => return Err(error.error),
        };
        let mut can_shrink = first_error.may_shrink();
        let pending_removals = self
            .memory_types
            .iter()
            .any(|ty| ty.pending_residency_removals);
        if self.trim() != 0 || pending_removals {
            attempted_size = self.memory_types[index].block_size(desc);
            match self.allocate_type(
                index,
                desc,
                #[cfg(feature = "std")]
                &backtrace,
                Some(attempted_size),
            ) {
                Ok(allocation) => return Ok(allocation),
                Err(error) if error.is_memory_pressure() => {
                    if error.is_allocation_limit() {
                        return Err(error.error);
                    }
                    can_shrink = error.may_shrink();
                }
                Err(error) => return Err(error.error),
            }
        }
        let minimum = size;
        if can_shrink
            && size <= self.memory_types[index].block_sizes.max
            && minimum < attempted_size
        {
            match self.allocate_type(
                index,
                desc,
                #[cfg(feature = "std")]
                &backtrace,
                Some(minimum),
            ) {
                Ok(allocation) => return Ok(allocation),
                Err(error) if error.is_memory_pressure() => {
                    if error.is_allocation_limit() {
                        return Err(error.error);
                    }
                }
                Err(error) => return Err(error.error),
            }
        }
        Err(first_error.error)
    }

    /// Returns the allocator capacity held by cached empty blocks.
    pub fn cached_capacity(&self) -> u64 {
        self.memory_types.iter().map(MemoryType::cached_size).sum()
    }

    fn commit_pending_removals(&mut self) {
        if self
            .memory_types
            .iter()
            .any(|ty| ty.pending_residency_removals)
        {
            if let Some(set) = &self.global_residency_set {
                set.commit();
            }
            for ty in &mut self.memory_types {
                ty.pending_residency_removals = false;
            }
        }
    }

    /// Releases cached empty blocks and returns bytes removed from allocator capacity.
    /// Live indices remain valid. This does not wait for GPU work or release external references.
    /// Applies pending changes to this allocator's residency set, including earlier removals.
    pub fn trim(&mut self) -> u64 {
        let released = self
            .memory_types
            .iter_mut()
            .map(|ty| ty.release_cached())
            .sum();
        self.commit_pending_removals();
        released
    }

    /// Releases largest cached blocks until their total capacity is at most `bytes`.
    /// The target applies to empty cached capacity, not live allocations or an OS memory budget.
    /// Applies pending changes to this allocator's residency set, including earlier removals.
    pub fn trim_to(&mut self, bytes: u64) -> u64 {
        let mut cached = self.cached_capacity();
        let before = cached;
        while cached > bytes {
            let Some((index, size)) = self
                .memory_types
                .iter()
                .enumerate()
                .map(|(index, ty)| (index, ty.cached_size()))
                .max_by_key(|(_, size)| *size)
                .filter(|(_, size)| *size != 0)
            else {
                break;
            };
            let released = self.memory_types[index].release_cached();
            debug_assert_eq!(released, size);
            if released == 0 {
                break;
            }
            cached -= released;
        }
        self.commit_pending_removals();
        before - cached
    }

    pub fn free(&mut self, allocation: &Allocation) -> Result<()> {
        if self.debug_settings.log_frees {
            let name = allocation.name.as_deref().unwrap_or("<null>");
            debug!("Freeing `{name}`.");
            #[cfg(feature = "std")]
            if self.debug_settings.log_stack_traces {
                let backtrace = Backtrace::force_capture();
                debug!("Free stack trace: {backtrace}");
            }
        }

        if allocation.is_null() {
            return Ok(());
        }
        self.memory_types[allocation.memory_type_index].free(allocation)?;
        Ok(())
    }

    /// Returns heaps for all memory blocks
    pub fn heaps(&self) -> impl Iterator<Item = &ProtocolObject<dyn MTLHeap>> {
        self.memory_types.iter().flat_map(|memory_type| {
            memory_type
                .memory_blocks
                .iter()
                .flatten()
                .map(|block| block.heap.as_ref())
        })
    }

    pub fn generate_report(&self) -> AllocatorReport {
        let mut allocations = vec![];
        let mut blocks = vec![];
        let mut total_capacity_bytes = 0;

        for memory_type in &self.memory_types {
            for block in memory_type.memory_blocks.iter().flatten() {
                total_capacity_bytes += block.size;
                let first_allocation = allocations.len();
                allocations.extend(block.sub_allocator.report_allocations());
                blocks.push(MemoryBlockReport {
                    size: block.size,
                    allocations: first_allocation..allocations.len(),
                });
            }
        }

        let total_allocated_bytes = allocations.iter().map(|report| report.size).sum();

        AllocatorReport {
            allocations,
            blocks,
            total_allocated_bytes,
            total_capacity_bytes,
        }
    }

    /// Current total capacity of memory blocks allocated on the device, in bytes
    pub fn capacity(&self) -> u64 {
        let mut total_capacity_bytes = 0;

        for memory_type in &self.memory_types {
            for block in memory_type.memory_blocks.iter().flatten() {
                total_capacity_bytes += block.size;
            }
        }

        total_capacity_bytes
    }

    /// Optional residency set containing all heap allocations created/owned by this allocator to
    /// be made resident at once when its allocations are used on the GPU.  The caller _must_ invoke
    /// [`MTLResidencySet::commit()`] whenever these resources are used to make sure the latest
    /// changes are visible to Metal, e.g. before committing a command buffer.
    ///
    /// This residency set can be attached to individual command buffers or to a queue directly
    /// since usage of allocated resources is expected to be global.
    ///
    /// Alternatively callers can build up their own residency set(s) based on individual
    /// [`MTLAllocation`]s [^heap-allocation] rather than making all heaps allocated via
    /// `gpu-allocator` resident at once.
    ///
    /// [^heap-allocation]: Note that [`MTLHeap`]s returned by [`Allocator::heaps()`] are also
    /// allocations.  If individual placed [`MTLResource`]s on a heap are made resident, the entire
    /// heap will be made resident.
    ///
    /// Callers still need to be careful to make resources created outside of `gpu-allocator`
    /// resident on the GPU, such as indirect command buffers.
    ///
    /// This residency set is only available when requested via
    /// [`AllocatorCreateDesc::create_residency_set`], otherwise this function returns [`None`].
    pub fn residency_set(&self) -> Option<&Retained<ProtocolObject<dyn MTLResidencySet>>> {
        // Return the retained object so that the caller also has a way to store it, since we will
        // keep using and updating the same object going forward.
        self.global_residency_set.as_ref()
    }
}
