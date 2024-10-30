use std::{backtrace::Backtrace, sync::Arc};

#[cfg(feature = "visualizer")]
mod visualizer;
#[cfg(feature = "visualizer")]
pub use visualizer::AllocatorVisualizer;

use log::debug;
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_foundation::NSString;
use objc2_metal::{
    MTLAccelerationStructure, MTLBuffer, MTLCPUCacheMode, MTLDevice, MTLHeap, MTLHeapDescriptor,
    MTLHeapType, MTLResource, MTLResourceOptions, MTLStorageMode, MTLTexture, MTLTextureDescriptor,
};

use crate::{
    allocator::{self, AllocatorReport, MemoryBlockReport},
    AllocationError, AllocationSizes, AllocatorDebugSettings, MemoryLocation, Result,
};

fn memory_location_to_metal(location: MemoryLocation) -> MTLResourceOptions {
    match location {
        MemoryLocation::GpuOnly => MTLResourceOptions::MTLResourceStorageModePrivate,
        MemoryLocation::CpuToGpu | MemoryLocation::GpuToCpu | MemoryLocation::Unknown => {
            MTLResourceOptions::MTLResourceStorageModeShared
        }
    }
}

#[derive(Debug)]
pub struct Allocation {
    chunk_id: Option<std::num::NonZeroU64>,
    offset: u64,
    size: u64,
    memory_block_index: usize,
    memory_type_index: usize,
    heap: Retained<ProtocolObject<dyn MTLHeap>>,
    name: Option<Box<str>>,
}

impl Allocation {
    pub fn heap(&self) -> &ProtocolObject<dyn MTLHeap> {
        &self.heap
    }

    pub fn make_buffer(&self) -> Option<Retained<ProtocolObject<dyn MTLBuffer>>> {
        let resource = unsafe {
            self.heap.newBufferWithLength_options_offset(
                self.size as usize,
                self.heap.resourceOptions(),
                self.offset as usize,
            )
        };
        if let Some(resource) = &resource {
            if let Some(name) = &self.name {
                resource.setLabel(Some(&NSString::from_str(name)));
            }
        }
        resource
    }

    pub fn make_texture(
        &self,
        desc: &MTLTextureDescriptor,
    ) -> Option<Retained<ProtocolObject<dyn MTLTexture>>> {
        let resource = unsafe {
            self.heap
                .newTextureWithDescriptor_offset(desc, self.offset as usize)
        };
        if let Some(resource) = &resource {
            if let Some(name) = &self.name {
                resource.setLabel(Some(&NSString::from_str(name)));
            }
        }
        resource
    }

    pub fn make_acceleration_structure(
        &self,
    ) -> Option<Retained<ProtocolObject<dyn MTLAccelerationStructure>>> {
        let resource = unsafe {
            self.heap
                .newAccelerationStructureWithSize_offset(self.size as usize, self.offset as usize)
        };
        if let Some(resource) = &resource {
            if let Some(name) = &self.name {
                resource.setLabel(Some(&NSString::from_str(name)));
            }
        }
        resource
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
    debug_settings: AllocatorDebugSettings,
    memory_types: Vec<MemoryType>,
    allocation_sizes: AllocationSizes,
}

impl std::fmt::Debug for Allocator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.generate_report().fmt(f)
    }
}

#[derive(Debug)]
pub struct AllocatorCreateDesc {
    pub device: Retained<ProtocolObject<dyn MTLDevice>>,
    pub debug_settings: AllocatorDebugSettings,
    pub allocation_sizes: AllocationSizes,
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
    sub_allocator: Box<dyn allocator::SubAllocator>,
}

impl MemoryBlock {
    fn new(
        device: &ProtocolObject<dyn MTLDevice>,
        size: u64,
        heap_descriptor: &MTLHeapDescriptor,
        dedicated: bool,
        memory_location: MemoryLocation,
    ) -> Result<Self> {
        heap_descriptor.setSize(size as usize);

        let heap = device
            .newHeapWithDescriptor(heap_descriptor)
            .ok_or_else(|| AllocationError::Internal("No MTLHeap was returned".to_string()))?;

        heap.setLabel(Some(&NSString::from_str(&format!(
            "MemoryBlock {memory_location:?}"
        ))));

        let sub_allocator: Box<dyn allocator::SubAllocator> = if dedicated {
            Box::new(allocator::DedicatedBlockAllocator::new(size))
        } else {
            Box::new(allocator::FreeListAllocator::new(size))
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
    memory_blocks: Vec<Option<MemoryBlock>>,
    _committed_allocations: CommittedAllocationStatistics,
    memory_location: MemoryLocation,
    heap_properties: Retained<MTLHeapDescriptor>,
    memory_type_index: usize,
    active_general_blocks: usize,
}

impl MemoryType {
    fn allocate(
        &mut self,
        device: &ProtocolObject<dyn MTLDevice>,
        desc: &AllocationCreateDesc<'_>,
        backtrace: Option<Arc<Backtrace>>,
        allocation_sizes: &AllocationSizes,
    ) -> Result<Allocation> {
        let allocation_type = allocator::AllocationType::Linear;

        let memblock_size = if self.heap_properties.storageMode() == MTLStorageMode::Private {
            allocation_sizes.device_memblock_size
        } else {
            allocation_sizes.host_memblock_size
        };

        let size = desc.size;
        let alignment = desc.alignment;

        // Create a dedicated block for large memory allocations
        if size > memblock_size {
            let mem_block = MemoryBlock::new(
                device,
                size,
                &self.heap_properties,
                true,
                self.memory_location,
            )?;

            let block_index = self.memory_blocks.iter().position(|block| block.is_none());
            let block_index = match block_index {
                Some(i) => {
                    self.memory_blocks[i].replace(mem_block);
                    i
                }
                None => {
                    self.memory_blocks.push(Some(mem_block));
                    self.memory_blocks.len() - 1
                }
            };

            let mem_block = self.memory_blocks[block_index]
                .as_mut()
                .ok_or_else(|| AllocationError::Internal("Memory block must be Some".into()))?;

            let (offset, chunk_id) = mem_block.sub_allocator.allocate(
                size,
                alignment,
                allocation_type,
                1,
                desc.name,
                backtrace,
            )?;

            return Ok(Allocation {
                chunk_id: Some(chunk_id),
                size,
                offset,
                memory_block_index: block_index,
                memory_type_index: self.memory_type_index,
                heap: mem_block.heap.clone(),
                name: Some(desc.name.into()),
            });
        }

        let mut empty_block_index = None;
        for (mem_block_i, mem_block) in self.memory_blocks.iter_mut().enumerate().rev() {
            if let Some(mem_block) = mem_block {
                let allocation = mem_block.sub_allocator.allocate(
                    size,
                    alignment,
                    allocation_type,
                    1,
                    desc.name,
                    backtrace.clone(),
                );

                match allocation {
                    Ok((offset, chunk_id)) => {
                        return Ok(Allocation {
                            chunk_id: Some(chunk_id),
                            offset,
                            size,
                            memory_block_index: mem_block_i,
                            memory_type_index: self.memory_type_index,
                            heap: mem_block.heap.clone(),
                            name: Some(desc.name.into()),
                        });
                    }
                    Err(AllocationError::OutOfMemory) => {} // Block is full, continue search.
                    Err(err) => return Err(err),            // Unhandled error, return.
                }
            } else if empty_block_index.is_none() {
                empty_block_index = Some(mem_block_i);
            }
        }

        let new_memory_block = MemoryBlock::new(
            device,
            memblock_size,
            &self.heap_properties,
            false,
            self.memory_location,
        )?;

        let new_block_index = if let Some(block_index) = empty_block_index {
            self.memory_blocks[block_index] = Some(new_memory_block);
            block_index
        } else {
            self.memory_blocks.push(Some(new_memory_block));
            self.memory_blocks.len() - 1
        };

        self.active_general_blocks += 1;

        let mem_block = self.memory_blocks[new_block_index]
            .as_mut()
            .ok_or_else(|| AllocationError::Internal("Memory block must be Some".into()))?;
        let allocation = mem_block.sub_allocator.allocate(
            size,
            alignment,
            allocation_type,
            1,
            desc.name,
            backtrace,
        );
        let (offset, chunk_id) = match allocation {
            Err(AllocationError::OutOfMemory) => Err(AllocationError::Internal(
                "Allocation that must succeed failed. This is a bug in the allocator.".into(),
            )),
            a => a,
        }?;

        Ok(Allocation {
            chunk_id: Some(chunk_id),
            offset,
            size,
            memory_block_index: new_block_index,
            memory_type_index: self.memory_type_index,
            heap: mem_block.heap.clone(),
            name: Some(desc.name.into()),
        })
    }

    fn free(&mut self, allocation: &Allocation) -> Result<()> {
        let block_idx = allocation.memory_block_index;

        let mem_block = self.memory_blocks[block_idx]
            .as_mut()
            .ok_or_else(|| AllocationError::Internal("Memory block must be Some.".into()))?;

        mem_block.sub_allocator.free(allocation.chunk_id)?;

        if mem_block.sub_allocator.is_empty() {
            if mem_block.sub_allocator.supports_general_allocations() {
                if self.active_general_blocks > 1 {
                    let block = self.memory_blocks[block_idx].take();
                    if block.is_none() {
                        return Err(AllocationError::Internal(
                            "Memory block must be Some.".into(),
                        ));
                    }
                    // Note that `block` will be destroyed on `drop` here

                    self.active_general_blocks -= 1;
                }
            } else {
                let block = self.memory_blocks[block_idx].take();
                if block.is_none() {
                    return Err(AllocationError::Internal(
                        "Memory block must be Some.".into(),
                    ));
                }
                // Note that `block` will be destroyed on `drop` here
            }
        }

        Ok(())
    }
}

impl Allocator {
    pub fn new(desc: &AllocatorCreateDesc) -> Result<Self> {
        let heap_types = [
            (MemoryLocation::GpuOnly, {
                let heap_desc = unsafe { MTLHeapDescriptor::new() };
                heap_desc.setCpuCacheMode(MTLCPUCacheMode::DefaultCache);
                heap_desc.setStorageMode(MTLStorageMode::Private);
                heap_desc.setType(MTLHeapType::Placement);
                heap_desc
            }),
            (MemoryLocation::CpuToGpu, {
                let heap_desc = unsafe { MTLHeapDescriptor::new() };
                heap_desc.setCpuCacheMode(MTLCPUCacheMode::WriteCombined);
                heap_desc.setStorageMode(MTLStorageMode::Shared);
                heap_desc.setType(MTLHeapType::Placement);
                heap_desc
            }),
            (MemoryLocation::GpuToCpu, {
                let heap_desc = unsafe { MTLHeapDescriptor::new() };
                heap_desc.setCpuCacheMode(MTLCPUCacheMode::DefaultCache);
                heap_desc.setStorageMode(MTLStorageMode::Shared);
                heap_desc.setType(MTLHeapType::Placement);
                heap_desc
            }),
        ];

        let memory_types = heap_types
            .into_iter()
            .enumerate()
            .map(|(i, (memory_location, heap_descriptor))| MemoryType {
                memory_blocks: vec![],
                _committed_allocations: CommittedAllocationStatistics {
                    num_allocations: 0,
                    total_size: 0,
                },
                memory_location,
                heap_properties: heap_descriptor,
                memory_type_index: i,
                active_general_blocks: 0,
            })
            .collect();

        Ok(Self {
            device: desc.device.clone(),
            debug_settings: desc.debug_settings,
            memory_types,
            allocation_sizes: desc.allocation_sizes,
        })
    }

    pub fn allocate(&mut self, desc: &AllocationCreateDesc<'_>) -> Result<Allocation> {
        let size = desc.size;
        let alignment = desc.alignment;

        let backtrace = if self.debug_settings.store_stack_traces {
            Some(Arc::new(Backtrace::force_capture()))
        } else {
            None
        };

        if self.debug_settings.log_allocations {
            debug!(
                "Allocating `{}` of {} bytes with an alignment of {}.",
                &desc.name, size, alignment
            );
            if self.debug_settings.log_stack_traces {
                let backtrace = Backtrace::force_capture();
                debug!("Allocation stack trace: {}", backtrace);
            }
        }

        if size == 0 || !alignment.is_power_of_two() {
            return Err(AllocationError::InvalidAllocationCreateDesc);
        }

        // Find memory type
        let memory_type = self
            .memory_types
            .iter_mut()
            .find(|memory_type| {
                // Is location compatible
                desc.location == MemoryLocation::Unknown
                    || desc.location == memory_type.memory_location
            })
            .ok_or(AllocationError::NoCompatibleMemoryTypeFound)?;

        memory_type.allocate(&self.device, desc, backtrace, &self.allocation_sizes)
    }

    pub fn free(&mut self, allocation: &Allocation) -> Result<()> {
        if self.debug_settings.log_frees {
            let name = allocation.name.as_deref().unwrap_or("<null>");
            debug!("Freeing `{}`.", name);
            if self.debug_settings.log_stack_traces {
                let backtrace = Backtrace::force_capture();
                debug!("Free stack trace: {}", backtrace);
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
        let mut total_reserved_bytes = 0;

        for memory_type in &self.memory_types {
            for block in memory_type.memory_blocks.iter().flatten() {
                total_reserved_bytes += block.size;
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
            total_reserved_bytes,
        }
    }
}
