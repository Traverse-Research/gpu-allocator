#![deny(clippy::unimplemented, clippy::unwrap_used, clippy::ok_expect)]

#[cfg(feature = "visualizer")]
mod visualizer;
#[cfg(feature = "visualizer")]
pub use visualizer::AllocatorVisualizer;

use super::allocator;
use super::allocator::AllocationType;
use ash::vk;
use log::{debug, warn, Level};
use std::fmt;

use crate::{
    allocator::fmt_bytes, AllocationError, AllocatorDebugSettings, MemoryLocation, Result,
};

#[derive(Clone, Debug)]
pub struct AllocationCreateDesc<'a> {
    /// Name of the allocation, for tracking and debugging purposes
    pub name: &'a str,
    /// Vulkan memory requirements for an allocation
    pub requirements: vk::MemoryRequirements,
    /// Location where the memory allocation should be stored
    pub location: MemoryLocation,
    /// If the resource is linear (buffer / linear texture) or a regular (tiled) texture.
    pub linear: bool,
}

pub struct AllocatorCreateDesc {
    pub instance: ash::Instance,
    pub device: ash::Device,
    pub physical_device: ash::vk::PhysicalDevice,
    pub debug_settings: AllocatorDebugSettings,
    pub buffer_device_address: bool,
}

#[derive(Debug)]
pub struct Allocation {
    chunk_id: Option<std::num::NonZeroU64>,
    offset: u64,
    size: u64,
    memory_block_index: usize,
    memory_type_index: usize,
    device_memory: vk::DeviceMemory,
    mapped_ptr: Option<std::ptr::NonNull<std::ffi::c_void>>,

    name: Option<Box<str>>,
}

// Sending is fine because mapped_ptr does not change based on the thread we are in
unsafe impl Send for Allocation {}
// Sync is also okay because Sending &Allocation is safe: a mutable reference
// to the data in mapped_ptr is never exposed while `self` is immutably borrowed.
// In order to break safety guarantees, the user needs to `unsafe`ly dereference
// `mapped_ptr` themselves.
unsafe impl Sync for Allocation {}

impl Allocation {
    pub fn chunk_id(&self) -> Option<std::num::NonZeroU64> {
        self.chunk_id
    }

    /// Returns the [`vk::DeviceMemory`] object that is backing this allocation.
    /// This memory object can be shared with multiple other allocations and shouldn't be freed (or allocated from)
    /// without this library, because that will lead to undefined behavior.
    ///
    /// # Safety
    /// The result of this function can safely be used to pass into [`ash::Device::bind_buffer_memory()`],
    /// [`ash::Device::bind_image_memory()`] etc. It is exposed for this reason. Keep in mind to also
    /// pass [`Self::offset()`] along to those.
    pub unsafe fn memory(&self) -> vk::DeviceMemory {
        self.device_memory
    }

    /// Returns the offset of the allocation on the [`vk::DeviceMemory`].
    /// When binding the memory to a buffer or image, this offset needs to be supplied as well.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Returns the size of the allocation
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Returns a valid mapped pointer if the memory is host visible, otherwise it will return None.
    /// The pointer already points to the exact memory region of the suballocation, so no offset needs to be applied.
    pub fn mapped_ptr(&self) -> Option<std::ptr::NonNull<std::ffi::c_void>> {
        self.mapped_ptr
    }

    /// Returns a valid mapped slice if the memory is host visible, otherwise it will return None.
    /// The slice already references the exact memory region of the allocation, so no offset needs to be applied.
    pub fn mapped_slice(&self) -> Option<&[u8]> {
        self.mapped_ptr().map(|ptr| unsafe {
            std::slice::from_raw_parts(ptr.cast().as_ptr(), self.size as usize)
        })
    }

    /// Returns a valid mapped mutable slice if the memory is host visible, otherwise it will return None.
    /// The slice already references the exact memory region of the allocation, so no offset needs to be applied.
    pub fn mapped_slice_mut(&mut self) -> Option<&mut [u8]> {
        self.mapped_ptr().map(|ptr| unsafe {
            std::slice::from_raw_parts_mut(ptr.cast().as_ptr(), self.size as usize)
        })
    }

    pub fn is_null(&self) -> bool {
        self.chunk_id.is_none()
    }
}

impl Default for Allocation {
    fn default() -> Self {
        Self {
            chunk_id: None,
            offset: 0,
            size: 0,
            memory_block_index: !0,
            memory_type_index: !0,
            device_memory: vk::DeviceMemory::null(),
            mapped_ptr: None,
            name: None,
        }
    }
}

#[derive(Debug)]
pub(crate) struct MemoryBlock {
    pub(crate) device_memory: vk::DeviceMemory,
    #[cfg(feature = "visualizer")]
    pub(crate) size: u64,
    pub(crate) mapped_ptr: *mut std::ffi::c_void,
    pub(crate) sub_allocator: Box<dyn allocator::SubAllocator>,
}

impl MemoryBlock {
    fn new(
        device: &ash::Device,
        size: u64,
        mem_type_index: usize,
        mapped: bool,
        dedicated: bool,
        buffer_device_address: bool,
    ) -> Result<Self> {
        let device_memory = {
            let alloc_info = vk::MemoryAllocateInfo::builder()
                .allocation_size(size)
                .memory_type_index(mem_type_index as u32);

            let allocation_flags = vk::MemoryAllocateFlags::DEVICE_ADDRESS;
            let mut flags_info = vk::MemoryAllocateFlagsInfo::builder().flags(allocation_flags);
            // TODO(manon): Test this based on if the device has this feature enabled or not
            let alloc_info = if buffer_device_address {
                alloc_info.push_next(&mut flags_info)
            } else {
                alloc_info
            };

            unsafe { device.allocate_memory(&alloc_info, None) }.map_err(|e| match e {
                vk::Result::ERROR_OUT_OF_DEVICE_MEMORY => AllocationError::OutOfMemory,
                e => AllocationError::Internal(format!(
                    "Unexpected error in vkAllocateMemory: {:?}",
                    e
                )),
            })?
        };

        let mapped_ptr = if mapped {
            unsafe {
                device.map_memory(
                    device_memory,
                    0,
                    vk::WHOLE_SIZE,
                    vk::MemoryMapFlags::empty(),
                )
            }
            .map_err(|e| {
                unsafe { device.free_memory(device_memory, None) };
                AllocationError::FailedToMap(e.to_string())
            })?
        } else {
            std::ptr::null_mut()
        };

        let sub_allocator: Box<dyn allocator::SubAllocator> = if dedicated {
            Box::new(allocator::DedicatedBlockAllocator::new(size))
        } else {
            Box::new(allocator::FreeListAllocator::new(size))
        };

        Ok(Self {
            device_memory,
            #[cfg(feature = "visualizer")]
            size,
            mapped_ptr,
            sub_allocator,
        })
    }

    fn destroy(self, device: &ash::Device) {
        if !self.mapped_ptr.is_null() {
            unsafe { device.unmap_memory(self.device_memory) };
        }

        unsafe { device.free_memory(self.device_memory, None) };
    }
}

// `mapped_ptr` is safe to send or share across threads because
// it is never exposed publicly through [`MemoryBlock`].
unsafe impl Send for MemoryBlock {}
unsafe impl Sync for MemoryBlock {}

#[derive(Debug)]
pub(crate) struct MemoryType {
    pub(crate) memory_blocks: Vec<Option<MemoryBlock>>,
    pub(crate) memory_properties: vk::MemoryPropertyFlags,
    pub(crate) memory_type_index: usize,
    pub(crate) heap_index: usize,
    pub(crate) mappable: bool,
    pub(crate) active_general_blocks: usize,
    pub(crate) buffer_device_address: bool,
}

const DEFAULT_DEVICE_MEMBLOCK_SIZE: u64 = 256 * 1024 * 1024;
const DEFAULT_HOST_MEMBLOCK_SIZE: u64 = 64 * 1024 * 1024;

impl MemoryType {
    fn allocate(
        &mut self,
        device: &ash::Device,
        desc: &AllocationCreateDesc<'_>,
        granularity: u64,
        backtrace: Option<backtrace::Backtrace>,
    ) -> Result<Allocation> {
        let allocation_type = if desc.linear {
            AllocationType::Linear
        } else {
            AllocationType::NonLinear
        };

        let memblock_size = if self
            .memory_properties
            .contains(vk::MemoryPropertyFlags::HOST_VISIBLE)
        {
            DEFAULT_HOST_MEMBLOCK_SIZE
        } else {
            DEFAULT_DEVICE_MEMBLOCK_SIZE
        };

        let size = desc.requirements.size;
        let alignment = desc.requirements.alignment;

        // Create a dedicated block for large memory allocations
        if size > memblock_size {
            let mem_block = MemoryBlock::new(
                device,
                size,
                self.memory_type_index,
                self.mappable,
                true,
                self.buffer_device_address,
            )?;

            let mut block_index = None;
            for (i, block) in self.memory_blocks.iter().enumerate() {
                if block.is_none() {
                    block_index = Some(i);
                    break;
                }
            }

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
                granularity,
                desc.name,
                backtrace,
            )?;

            return Ok(Allocation {
                chunk_id: Some(chunk_id),
                offset,
                size,
                memory_block_index: block_index,
                memory_type_index: self.memory_type_index,
                device_memory: mem_block.device_memory,
                mapped_ptr: std::ptr::NonNull::new(mem_block.mapped_ptr),
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
                    granularity,
                    desc.name,
                    backtrace.clone(),
                );

                match allocation {
                    Ok((offset, chunk_id)) => {
                        let mapped_ptr = if !mem_block.mapped_ptr.is_null() {
                            let offset_ptr = unsafe { mem_block.mapped_ptr.add(offset as usize) };
                            std::ptr::NonNull::new(offset_ptr)
                        } else {
                            None
                        };
                        return Ok(Allocation {
                            chunk_id: Some(chunk_id),
                            offset,
                            size,
                            memory_block_index: mem_block_i,
                            memory_type_index: self.memory_type_index,
                            device_memory: mem_block.device_memory,
                            mapped_ptr,
                            name: Some(desc.name.into()),
                        });
                    }
                    Err(err) => match err {
                        AllocationError::OutOfMemory => {} // Block is full, continue search.
                        _ => return Err(err),              // Unhandled error, return.
                    },
                }
            } else if empty_block_index == None {
                empty_block_index = Some(mem_block_i);
            }
        }

        let new_memory_block = MemoryBlock::new(
            device,
            memblock_size,
            self.memory_type_index,
            self.mappable,
            false,
            self.buffer_device_address,
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
            granularity,
            desc.name,
            backtrace,
        );
        let (offset, chunk_id) = match allocation {
            Ok(value) => value,
            Err(err) => match err {
                AllocationError::OutOfMemory => {
                    return Err(AllocationError::Internal(
                        "Allocation that must succeed failed. This is a bug in the allocator."
                            .into(),
                    ))
                }
                _ => return Err(err),
            },
        };

        let mapped_ptr = if !mem_block.mapped_ptr.is_null() {
            let offset_ptr = unsafe { mem_block.mapped_ptr.add(offset as usize) };
            std::ptr::NonNull::new(offset_ptr)
        } else {
            None
        };

        Ok(Allocation {
            chunk_id: Some(chunk_id),
            offset,
            size,
            memory_block_index: new_block_index,
            memory_type_index: self.memory_type_index,
            device_memory: mem_block.device_memory,
            mapped_ptr,
            name: Some(desc.name.into()),
        })
    }

    #[allow(clippy::needless_pass_by_value)]
    fn free(&mut self, allocation: Allocation, device: &ash::Device) -> Result<()> {
        let block_idx = allocation.memory_block_index;

        let mem_block = self.memory_blocks[block_idx]
            .as_mut()
            .ok_or_else(|| AllocationError::Internal("Memory block must be Some.".into()))?;

        mem_block.sub_allocator.free(allocation.chunk_id)?;

        if mem_block.sub_allocator.is_empty() {
            if mem_block.sub_allocator.supports_general_allocations() {
                if self.active_general_blocks > 1 {
                    let block = self.memory_blocks[block_idx].take();
                    let block = block.ok_or_else(|| {
                        AllocationError::Internal("Memory block must be Some.".into())
                    })?;
                    block.destroy(device);

                    self.active_general_blocks -= 1;
                }
            } else {
                let block = self.memory_blocks[block_idx].take();
                let block = block.ok_or_else(|| {
                    AllocationError::Internal("Memory block must be Some.".into())
                })?;
                block.destroy(device);
            }
        }

        Ok(())
    }
}

pub struct Allocator {
    pub(crate) memory_types: Vec<MemoryType>,
    pub(crate) memory_heaps: Vec<vk::MemoryHeap>,
    device: ash::Device,
    pub(crate) buffer_image_granularity: u64,
    pub(crate) debug_settings: AllocatorDebugSettings,
}

impl fmt::Debug for Allocator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut allocation_report = vec![];

        for memory_type in &self.memory_types {
            for block in memory_type.memory_blocks.iter().flatten() {
                allocation_report.extend_from_slice(&block.sub_allocator.report_allocations())
            }
        }

        let total_size_in_bytes = allocation_report.iter().map(|report| report.size).sum();

        let mut allocation_report = allocation_report.iter().enumerate().collect::<Vec<_>>();
        allocation_report.sort_by_key(|(_, alloc)| std::cmp::Reverse(alloc.size));

        const MAX_NUM_CHARACTERS: usize = 40;

        f.write_str("================================================================\n")?;
        writeln!(
            f,
            "ALLOCATION BREAKDOWN ({})",
            fmt_bytes(total_size_in_bytes)
        )?;

        for (idx, alloc) in &allocation_report {
            let mut cloned_name = alloc.name.clone();
            cloned_name.truncate(MAX_NUM_CHARACTERS);

            let num_spaces = MAX_NUM_CHARACTERS - cloned_name.len();
            let aligning_spaces = " ".repeat(num_spaces);

            writeln!(
                f,
                "\t\t{}\t- {}{}\t- {}",
                idx,
                cloned_name,
                aligning_spaces,
                fmt_bytes(alloc.size)
            )?;
        }

        Ok(())
    }
}

impl Allocator {
    pub fn new(desc: &AllocatorCreateDesc) -> Result<Self> {
        if desc.physical_device == ash::vk::PhysicalDevice::null() {
            return Err(AllocationError::InvalidAllocatorCreateDesc(
                "AllocatorCreateDesc field `physical_device` is null.".into(),
            ));
        }

        let mem_props = unsafe {
            desc.instance
                .get_physical_device_memory_properties(desc.physical_device)
        };

        let memory_types = &mem_props.memory_types[..mem_props.memory_type_count as _];
        let memory_heaps = mem_props.memory_heaps[..mem_props.memory_heap_count as _].to_vec();

        if desc.debug_settings.log_memory_information {
            debug!("memory type count: {}", mem_props.memory_type_count);
            debug!("memory heap count: {}", mem_props.memory_heap_count);

            for (i, mem_type) in memory_types.iter().enumerate() {
                let flags = mem_type.property_flags;
                debug!(
                    "memory type[{}]: prop flags: 0x{:x}, heap[{}]",
                    i,
                    flags.as_raw(),
                    mem_type.heap_index,
                );
            }
            for (i, heap) in memory_heaps.iter().enumerate() {
                debug!(
                    "heap[{}] flags: 0x{:x}, size: {} MiB",
                    i,
                    heap.flags.as_raw(),
                    heap.size / (1024 * 1024)
                );
            }
        }

        // NOTE(manon): Test if there is any HOST_VISIBLE memory that does _not_
        //            have the HOST_COHERENT flag, in that case we want to panic,
        //            as we want to do cool things that we do not yet support
        //            with that type of memory :)
        let host_visible_not_coherent = memory_types.iter().any(|t| {
            let flags = t.property_flags;
            flags.contains(vk::MemoryPropertyFlags::HOST_VISIBLE)
                && !flags.contains(vk::MemoryPropertyFlags::HOST_COHERENT)
        });
        if host_visible_not_coherent {
            warn!("There is a memory type that is host visible, but not host coherent. It's time to upgrade our memory allocator to take advantage of this type of memory :)");
        }

        let memory_types = memory_types
            .iter()
            .enumerate()
            .map(|(i, mem_type)| MemoryType {
                memory_blocks: Vec::default(),
                memory_properties: mem_type.property_flags,
                memory_type_index: i,
                heap_index: mem_type.heap_index as usize,
                mappable: mem_type
                    .property_flags
                    .contains(vk::MemoryPropertyFlags::HOST_VISIBLE),
                active_general_blocks: 0,
                buffer_device_address: desc.buffer_device_address,
            })
            .collect::<Vec<_>>();

        let physical_device_properties = unsafe {
            desc.instance
                .get_physical_device_properties(desc.physical_device)
        };

        let granularity = physical_device_properties.limits.buffer_image_granularity;

        Ok(Self {
            memory_types,
            memory_heaps,
            device: desc.device.clone(),
            buffer_image_granularity: granularity,
            debug_settings: desc.debug_settings,
        })
    }

    pub fn allocate(&mut self, desc: &AllocationCreateDesc<'_>) -> Result<Allocation> {
        let size = desc.requirements.size;
        let alignment = desc.requirements.alignment;

        let backtrace = if self.debug_settings.store_stack_traces {
            Some(backtrace::Backtrace::new_unresolved())
        } else {
            None
        };

        if self.debug_settings.log_allocations {
            debug!(
                "Allocating `{}` of {} bytes with an alignment of {}.",
                &desc.name, size, alignment
            );
            if self.debug_settings.log_stack_traces {
                let backtrace = backtrace::Backtrace::new();
                debug!("Allocation stack trace: {:?}", backtrace);
            }
        }

        if size == 0 || !alignment.is_power_of_two() {
            return Err(AllocationError::InvalidAllocationCreateDesc);
        }

        let mem_loc_preferred_bits = match desc.location {
            MemoryLocation::GpuOnly => vk::MemoryPropertyFlags::DEVICE_LOCAL,
            MemoryLocation::CpuToGpu => {
                vk::MemoryPropertyFlags::HOST_VISIBLE
                    | vk::MemoryPropertyFlags::HOST_COHERENT
                    | vk::MemoryPropertyFlags::DEVICE_LOCAL
            }
            MemoryLocation::GpuToCpu => {
                vk::MemoryPropertyFlags::HOST_VISIBLE
                    | vk::MemoryPropertyFlags::HOST_COHERENT
                    | vk::MemoryPropertyFlags::HOST_CACHED
            }
            MemoryLocation::Unknown => vk::MemoryPropertyFlags::empty(),
        };
        let mut memory_type_index_opt =
            self.find_memorytype_index(&desc.requirements, mem_loc_preferred_bits);

        if memory_type_index_opt.is_none() {
            let mem_loc_required_bits = match desc.location {
                MemoryLocation::GpuOnly => vk::MemoryPropertyFlags::DEVICE_LOCAL,
                MemoryLocation::CpuToGpu | MemoryLocation::GpuToCpu => {
                    vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT
                }
                MemoryLocation::Unknown => vk::MemoryPropertyFlags::empty(),
            };

            memory_type_index_opt =
                self.find_memorytype_index(&desc.requirements, mem_loc_required_bits);
        }

        let memory_type_index = match memory_type_index_opt {
            Some(x) => x as usize,
            None => return Err(AllocationError::NoCompatibleMemoryTypeFound),
        };

        //Do not try to create a block if the heap is smaller than the required size (avoids validation warnings).
        let memory_type = &mut self.memory_types[memory_type_index];
        let allocation = if size > self.memory_heaps[memory_type.heap_index].size {
            Err(AllocationError::OutOfMemory)
        } else {
            memory_type.allocate(
                &self.device,
                desc,
                self.buffer_image_granularity,
                backtrace.clone(),
            )
        };

        if desc.location == MemoryLocation::CpuToGpu {
            if allocation.is_err() {
                let mem_loc_preferred_bits =
                    vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;

                let memory_type_index_opt =
                    self.find_memorytype_index(&desc.requirements, mem_loc_preferred_bits);

                let memory_type_index = match memory_type_index_opt {
                    Some(x) => x as usize,
                    None => return Err(AllocationError::NoCompatibleMemoryTypeFound),
                };

                self.memory_types[memory_type_index].allocate(
                    &self.device,
                    desc,
                    self.buffer_image_granularity,
                    backtrace,
                )
            } else {
                allocation
            }
        } else {
            allocation
        }
    }

    pub fn free(&mut self, allocation: Allocation) -> Result<()> {
        if self.debug_settings.log_frees {
            let name = allocation.name.as_deref().unwrap_or("<null>");
            debug!("Freeing `{}`.", name);
            if self.debug_settings.log_stack_traces {
                let backtrace = format!("{:?}", backtrace::Backtrace::new());
                debug!("Free stack trace: {}", backtrace);
            }
        }

        if allocation.is_null() {
            return Ok(());
        }

        self.memory_types[allocation.memory_type_index].free(allocation, &self.device)?;

        Ok(())
    }

    pub fn rename_allocation(&mut self, allocation: &mut Allocation, name: &str) -> Result<()> {
        allocation.name = Some(name.into());

        if allocation.is_null() {
            return Ok(());
        }

        let mem_type = &mut self.memory_types[allocation.memory_type_index];
        let mem_block = mem_type.memory_blocks[allocation.memory_block_index]
            .as_mut()
            .ok_or_else(|| AllocationError::Internal("Memory block must be Some.".into()))?;

        mem_block
            .sub_allocator
            .rename_allocation(allocation.chunk_id, name)?;

        Ok(())
    }

    pub fn report_memory_leaks(&self, log_level: Level) {
        for (mem_type_i, mem_type) in self.memory_types.iter().enumerate() {
            for (block_i, mem_block) in mem_type.memory_blocks.iter().enumerate() {
                if let Some(mem_block) = mem_block {
                    mem_block
                        .sub_allocator
                        .report_memory_leaks(log_level, mem_type_i, block_i);
                }
            }
        }
    }

    fn find_memorytype_index(
        &self,
        memory_req: &vk::MemoryRequirements,
        flags: vk::MemoryPropertyFlags,
    ) -> Option<u32> {
        self.memory_types
            .iter()
            .find(|memory_type| {
                (1 << memory_type.memory_type_index) & memory_req.memory_type_bits != 0
                    && memory_type.memory_properties.contains(flags)
            })
            .map(|memory_type| memory_type.memory_type_index as _)
    }
}

impl Drop for Allocator {
    fn drop(&mut self) {
        if self.debug_settings.log_leaks_on_shutdown {
            self.report_memory_leaks(Level::Warn);
        }

        // Free all remaining memory blocks
        for mem_type in self.memory_types.iter_mut() {
            for mem_block in mem_type.memory_blocks.iter_mut() {
                let block = mem_block.take();
                if let Some(block) = block {
                    block.destroy(&self.device);
                }
            }
        }
    }
}
