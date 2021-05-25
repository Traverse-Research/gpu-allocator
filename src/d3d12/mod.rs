#![deny(clippy::unimplemented, clippy::unwrap_used, clippy::ok_expect)]
use log::{log, Level};
use winapi::um::d3d12;

use super::allocator;
use super::allocator::AllocationType;

use crate::{AllocationError, AllocatorDebugSettings, MemoryLocation, Result};

#[derive(Clone, Debug)]
pub struct AllocationCreateDesc<'a> {
    /// Name of the allocation, for tracking and debugging purposes
    pub name: &'a str,
    // Should we use the D3D12 struct here?
    pub size: u64,
    pub alignment: u64,
    /// Location where the memory allocation should be stored
    pub location: MemoryLocation,
    /// If the resource is linear (buffer / linear texture) or a regular (tiled) texture.
    pub linear: bool,
}

pub struct AllocatorCreateDesc {
    pub device: *mut d3d12::ID3D12Device,
    pub debug_settings: AllocatorDebugSettings,
}

#[derive(Clone, Debug)]
pub struct Allocation {
    chunk_id: Option<std::num::NonZeroU64>,
    offset: u64,
    size: u64,
    memory_block_index: usize,
    memory_type_index: usize,
    device_memory: *mut d3d12::ID3D12Heap,
    mapped_ptr: Option<std::ptr::NonNull<std::ffi::c_void>>,

    name: Option<String>,
    backtrace: Option<String>,
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

    /// Returns the `vk::DeviceMemory` object that is backing this allocation.
    /// This memory object can be shared with multiple other allocations and shouldn't be free'd (or allocated from)
    /// without this library, because that will lead to undefined behavior.
    ///
    /// # Safety
    /// The result of this function can safely be used to pass into `bind_buffer_memory` (`vkBindBufferMemory`),
    /// `bind_texture_memory` (`vkBindTextureMemory`) etc. It's exposed for this reason. Keep in mind to also
    /// pass `Self::offset()` along to those.
    pub unsafe fn memory(&self) -> *mut d3d12::ID3D12Heap {
        self.device_memory
    }

    /// Returns the offset of the allocation on the vk::DeviceMemory.
    /// When binding the memory to a buffer or image, this offset needs to be supplied as well.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Returns the size of the allocation
    pub fn size(&self) -> u64 {
        self.size
    }

    /// # Safety
    /// Be careful not to mutably alias with this pointer; safety cannot be guaranteed, particularly over multiple threads.
    pub unsafe fn mapped_ptr(&self) -> Option<std::ptr::NonNull<std::ffi::c_void>> {
        self.mapped_ptr
    }

    /// Returns a valid mapped slice if the memory is host visible, otherwise it will return None.
    /// The slice already references the exact memory region of the allocation, so no offset needs to be applied.
    pub fn mapped_slice(&self) -> Option<&[u8]> {
        self.mapped_ptr.map(|ptr| unsafe {
            std::slice::from_raw_parts(ptr.as_ptr() as *const _, self.size() as usize)
        })
    }

    /// Returns a valid mapped mutable slice if the memory is host visible, otherwise it will return None.
    /// The slice already references the exact memory region of the allocation, so no offset needs to be applied.
    pub fn mapped_slice_mut(&mut self) -> Option<&mut [u8]> {
        self.mapped_ptr.map(|ptr| unsafe {
            std::slice::from_raw_parts_mut(ptr.as_ptr() as *mut _, self.size() as usize)
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
            device_memory: std::ptr::null_mut(),
            mapped_ptr: None,
            name: None,
            backtrace: None,
        }
    }
}

struct MemoryBlock {
    device_memory: *mut d3d12::ID3D12Heap,
    sub_allocator: Box<dyn allocator::SubAllocator>,
}
impl MemoryBlock {
    fn new(
        device: &mut d3d12::ID3D12Device,
        size: u64,
        heap_properties: &d3d12::D3D12_HEAP_PROPERTIES,
        dedicated: bool,
    ) -> Result<Self> {
        let device_memory = unsafe {
            let mut desc = d3d12::D3D12_HEAP_DESC::default();
            desc.SizeInBytes = size;
            desc.Properties = *heap_properties;
            desc.Alignment = d3d12::D3D12_DEFAULT_MSAA_RESOURCE_PLACEMENT_ALIGNMENT as u64;
            desc.Flags = d3d12::D3D12_HEAP_FLAG_NONE;

            let mut heap: *mut d3d12::ID3D12Heap = std::ptr::null_mut();

            let hr =
                device.CreateHeap(&desc, &d3d12::IID_ID3D12Heap, &mut heap as *mut _ as *mut _);
            assert_eq!(
                //TODO(max): Return error
                hr,
                winapi::shared::winerror::S_OK,
                "Failed to allocate ID3D12Heap of {} bytes",
                size,
            );
            heap
        };

        let sub_allocator: Box<dyn allocator::SubAllocator> = if dedicated {
            Box::new(allocator::DedicatedBlockAllocator::new(size))
        } else {
            Box::new(allocator::FreeListAllocator::new(size))
        };

        //TODO(max): Create placed resource to map heap

        Ok(Self {
            device_memory,
            sub_allocator,
        })
    }

    fn destroy(self) {
        unsafe { self.device_memory.as_mut().unwrap().Release() };
    }
}

// `mapped_ptr` is safe to send or share across threads because
// it is never exposed publicly through [`MemoryBlock`].
unsafe impl Send for MemoryBlock {}
unsafe impl Sync for MemoryBlock {}

#[cfg(windows)]
struct MemoryType {
    memory_blocks: Vec<Option<MemoryBlock>>,
    memory_properties: d3d12::D3D12_HEAP_PROPERTIES,
    memory_type_index: usize,
    active_general_blocks: usize,
}

const DEFAULT_DEVICE_MEMBLOCK_SIZE: u64 = 256 * 1024 * 1024;
const DEFAULT_HOST_MEMBLOCK_SIZE: u64 = 64 * 1024 * 1024;
#[cfg(windows)]
impl MemoryType {
    fn allocate(
        &mut self,
        device: &mut d3d12::ID3D12Device,
        desc: &AllocationCreateDesc,
        granularity: u64,
        backtrace: Option<&str>,
    ) -> Result<Allocation> {
        let allocation_type = if desc.linear {
            AllocationType::Linear
        } else {
            AllocationType::NonLinear
        };

        let memblock_size = if self.memory_properties.Type == d3d12::D3D12_HEAP_TYPE_DEFAULT {
            DEFAULT_DEVICE_MEMBLOCK_SIZE
        } else {
            DEFAULT_HOST_MEMBLOCK_SIZE
        };

        let size = desc.size;
        let alignment = desc.alignment;

        // Create a dedicated block for large memory allocations
        if size > memblock_size {
            let mem_block = MemoryBlock::new(device, size, &self.memory_properties, true)?;

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
                size,
                offset,
                memory_block_index: block_index,
                memory_type_index: self.memory_type_index as usize,
                device_memory: mem_block.device_memory,
                mapped_ptr: None,
                name: Some(desc.name.to_owned()),
                backtrace: backtrace.map(|s| s.to_owned()),
                ..Allocation::default()
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
                    backtrace,
                );

                match allocation {
                    Ok((offset, chunk_id)) => {
                        return Ok(Allocation {
                            chunk_id: Some(chunk_id),
                            offset,
                            size,
                            memory_block_index: mem_block_i,
                            memory_type_index: self.memory_type_index as usize,
                            device_memory: mem_block.device_memory,
                            mapped_ptr: None,
                            name: Some(desc.name.to_owned()),
                            backtrace: backtrace.map(|s| s.to_owned()),
                            ..Default::default()
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

        let new_memory_block =
            MemoryBlock::new(device, memblock_size, &self.memory_properties, false)?;

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
            .ok_or_else(|| AllocationError::Internal("memory block must be Some".into()))?;
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

        Ok(Allocation {
            chunk_id: Some(chunk_id),
            offset,
            size,
            memory_block_index: new_block_index,
            memory_type_index: self.memory_type_index as usize,
            device_memory: mem_block.device_memory,
            mapped_ptr: None,
            name: Some(desc.name.to_owned()),
            backtrace: backtrace.map(|s| s.to_owned()),
            ..Default::default()
        })
    }

    fn free(&mut self, allocation: Allocation) -> Result<()> {
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
                    block.destroy();

                    self.active_general_blocks -= 1;
                }
            } else {
                let block = self.memory_blocks[block_idx].take();
                let block = block.ok_or_else(|| {
                    AllocationError::Internal("Memory block must be Some.".into())
                })?;
                block.destroy();
            }
        }

        Ok(())
    }
}

pub struct Allocator {
    device: *mut d3d12::ID3D12Device,
    debug_settings: AllocatorDebugSettings,
    memory_types: Vec<MemoryType>,
}

impl Allocator {
    pub fn new(desc: &AllocatorCreateDesc) -> Self {
        let heap_types = [
            d3d12::D3D12_HEAP_PROPERTIES {
                Type: d3d12::D3D12_HEAP_TYPE_DEFAULT,
                CreationNodeMask: 1,
                VisibleNodeMask: 1,
                ..Default::default()
            },
            d3d12::D3D12_HEAP_PROPERTIES {
                Type: d3d12::D3D12_HEAP_TYPE_CUSTOM,
                CPUPageProperty: d3d12::D3D12_CPU_PAGE_PROPERTY_WRITE_COMBINE,
                MemoryPoolPreference: d3d12::D3D12_MEMORY_POOL_L0,
                CreationNodeMask: 1,
                VisibleNodeMask: 1,
            },
            d3d12::D3D12_HEAP_PROPERTIES {
                Type: d3d12::D3D12_HEAP_TYPE_CUSTOM,
                CPUPageProperty: d3d12::D3D12_CPU_PAGE_PROPERTY_WRITE_BACK,
                MemoryPoolPreference: d3d12::D3D12_MEMORY_POOL_L0,
                CreationNodeMask: 1,
                VisibleNodeMask: 1,
            },
        ];

        let memory_types = heap_types
            .iter()
            .enumerate()
            .map(|(i, &memory_properties)| MemoryType {
                memory_blocks: Vec::default(),
                memory_properties,
                memory_type_index: i,
                active_general_blocks: 0,
            })
            .collect::<Vec<_>>();

        Self {
            memory_types,
            device: desc.device.clone(),
            debug_settings: desc.debug_settings,
        }
    }

    pub fn allocate(&mut self, desc: &AllocationCreateDesc) -> Result<Allocation> {
        let size = desc.size;
        let alignment = desc.alignment;

        let backtrace = if self.debug_settings.store_stack_traces {
            Some(format!("{:?}", backtrace::Backtrace::new()))
        } else {
            None
        };

        if self.debug_settings.log_allocations {
            log!(
                Level::Debug,
                "Allocating \"{}\" of {} bytes with an alignment of {}.",
                &desc.name,
                size,
                alignment
            );
            if self.debug_settings.log_stack_traces {
                let backtrace = backtrace
                    .clone()
                    .unwrap_or(format!("{:?}", backtrace::Backtrace::new()));
                log!(Level::Debug, "Allocation stack trace: {}", &backtrace);
            }
        }

        if size == 0 || !alignment.is_power_of_two() {
            return Err(AllocationError::InvalidAllocationCreateDesc);
        }

        let memory_type_index = match desc.location {
            MemoryLocation::GpuOnly => 0,
            MemoryLocation::CpuToGpu => 1,
            MemoryLocation::GpuToCpu => 2,
            MemoryLocation::Unknown => panic!("Not sure what to do with unknown at the moment"),
        };

        self.memory_types[memory_type_index].allocate(
            unsafe { self.device.as_mut().unwrap() },
            desc,
            256, //TODO(max): Is this even a thing in D3D12?
            backtrace.as_deref(),
        )
    }

    pub fn free(&mut self, allocation: Allocation) -> Result<()> {
        if self.debug_settings.log_frees {
            let name = allocation.name.as_deref().unwrap_or("<null>");
            log!(Level::Debug, "Free'ing \"{}\".", name);
            if self.debug_settings.log_stack_traces {
                let backtrace = format!("{:?}", backtrace::Backtrace::new());
                log!(Level::Debug, "Free stack trace: {}", backtrace);
            }
        }

        if allocation.is_null() {
            return Ok(());
        }

        self.memory_types[allocation.memory_type_index].free(allocation)?;

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
}

#[cfg(windows)]
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
                    block.destroy();
                }
            }
        }
    }
}
