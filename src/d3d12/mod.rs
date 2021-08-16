#![deny(clippy::unimplemented, clippy::unwrap_used, clippy::ok_expect)]
use log::{log, Level};
use winapi::shared::winerror;
use winapi::um::d3d12;

#[cfg(feature = "visualizer")]
mod visualizer;
#[cfg(feature = "visualizer")]
pub use visualizer::AllocatorVisualizer;

use super::allocator;
use super::allocator::AllocationType;

use crate::{AllocationError, AllocatorDebugSettings, MemoryLocation, Result};

///
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceCategory {
    Buffer,
    RtvDsvTexture,
    OtherTexture,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HeapCategory {
    All,
    Buffer,
    RtvDsvTexture,
    OtherTexture,
}

impl From<ResourceCategory> for HeapCategory {
    fn from(resource_category: ResourceCategory) -> Self {
        match resource_category {
            ResourceCategory::Buffer => HeapCategory::Buffer,
            ResourceCategory::RtvDsvTexture => HeapCategory::RtvDsvTexture,
            ResourceCategory::OtherTexture => HeapCategory::OtherTexture,
        }
    }
}

impl From<&d3d12::D3D12_RESOURCE_DESC> for ResourceCategory {
    fn from(desc: &d3d12::D3D12_RESOURCE_DESC) -> Self {
        if desc.Dimension == d3d12::D3D12_RESOURCE_DIMENSION_BUFFER {
            ResourceCategory::Buffer
        } else if (desc.Flags
            & (d3d12::D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET
                | d3d12::D3D12_RESOURCE_FLAG_ALLOW_DEPTH_STENCIL))
            != 0
        {
            ResourceCategory::RtvDsvTexture
        } else {
            ResourceCategory::OtherTexture
        }
    }
}

#[derive(Clone, Debug)]
pub struct AllocationCreateDesc<'a> {
    /// Name of the allocation, for tracking and debugging purposes
    pub name: &'a str,
    /// Location where the memory allocation should be stored
    pub location: MemoryLocation,

    /// Size of allocation, should be queried using `ID3D12Device::GetResourceAllocationInfo`
    pub size: u64,
    /// Alignment of allocation, should be queried using `ID3D12Device::GetResourceAllocationInfo`
    pub alignment: u64,
    /// Resource category based on resource dimension and flags. Can be created from a `D3D12_RESOURCE_DESC`
    /// using the helper into function. The resource category is ignored when Resource Heap Tier 2 or higher
    /// is supported.
    pub resource_category: ResourceCategory,
}

impl<'a> AllocationCreateDesc<'a> {
    #[cfg(feature = "winapi")]
    pub fn from_d3d12_resource_desc(
        device: &d3d12::ID3D12Device,
        desc: &d3d12::D3D12_RESOURCE_DESC,
        name: &'a str,
        location: MemoryLocation,
    ) -> AllocationCreateDesc<'a> {
        let allocation_info = unsafe { device.GetResourceAllocationInfo(0, 1, desc as *const _) };
        let resource_category: ResourceCategory = desc.into();

        AllocationCreateDesc {
            name,
            location,
            size: allocation_info.SizeInBytes,
            alignment: allocation_info.Alignment,
            resource_category,
        }
    }
}

#[derive(Debug)]
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
    heap: *mut d3d12::ID3D12Heap,

    name: Option<String>,
    backtrace: Option<String>,
}

unsafe impl Send for Allocation {}
unsafe impl Sync for Allocation {}

impl Allocation {
    pub fn chunk_id(&self) -> Option<std::num::NonZeroU64> {
        self.chunk_id
    }

    /// Returns the `d3d12::ID3D12Heap` object that is backing this allocation.
    /// This heap object can be shared with multiple other allocations and shouldn't be freed (or allocated from)
    /// without this library, because that will lead to undefined behavior.
    ///
    /// # Safety
    /// The result of this function can safely be used to pass into `CreatePlacedResource`. It's exposed
    /// for this reason. Keep in mind to also pass `Self::offset()` along to it.
    pub unsafe fn heap(&self) -> *mut d3d12::ID3D12Heap {
        self.heap
    }

    /// Returns the offset of the allocation on the ID3D12Heap.
    /// When creating a placed resources, this offset needs to be supplied as well.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Returns the size of the allocation
    pub fn size(&self) -> u64 {
        self.size
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
            heap: std::ptr::null_mut(),
            name: None,
            backtrace: None,
        }
    }
}

struct MemoryBlock {
    heap: std::ptr::NonNull<d3d12::ID3D12Heap>,
    sub_allocator: Box<dyn allocator::SubAllocator>,
}
impl MemoryBlock {
    fn new(
        device: &mut d3d12::ID3D12Device,
        size: u64,
        heap_properties: &d3d12::D3D12_HEAP_PROPERTIES,
        heap_category: HeapCategory,
        dedicated: bool,
    ) -> Result<Self> {
        let heap = unsafe {
            let mut desc = d3d12::D3D12_HEAP_DESC {
                SizeInBytes: size,
                Properties: *heap_properties,
                Alignment: d3d12::D3D12_DEFAULT_MSAA_RESOURCE_PLACEMENT_ALIGNMENT as u64,
                ..Default::default()
            };
            desc.Flags = match heap_category {
                HeapCategory::All => d3d12::D3D12_HEAP_FLAG_NONE,
                HeapCategory::Buffer => d3d12::D3D12_HEAP_FLAG_ALLOW_ONLY_BUFFERS,
                HeapCategory::RtvDsvTexture => d3d12::D3D12_HEAP_FLAG_ALLOW_ONLY_RT_DS_TEXTURES,
                HeapCategory::OtherTexture => d3d12::D3D12_HEAP_FLAG_ALLOW_ONLY_NON_RT_DS_TEXTURES,
            };

            let mut heap: *mut d3d12::ID3D12Heap = std::ptr::null_mut();

            let hr =
                device.CreateHeap(&desc, &d3d12::IID_ID3D12Heap, &mut heap as *mut _ as *mut _);

            assert_eq!(
                //TODO(max): Return error
                hr,
                winerror::S_OK,
                "Failed to allocate ID3D12Heap of {} bytes",
                size,
            );

            //TODO(max): What type of error should this be? It's more like an OOM error
            std::ptr::NonNull::new(heap)
                .ok_or_else(|| AllocationError::Internal("Failed to create ID3D12Heap".into()))?
        };

        let sub_allocator: Box<dyn allocator::SubAllocator> = if dedicated {
            Box::new(allocator::DedicatedBlockAllocator::new(size))
        } else {
            Box::new(allocator::FreeListAllocator::new(size))
        };

        //TODO(max): Create placed resource to map heap

        Ok(Self {
            heap,
            sub_allocator,
        })
    }

    fn destroy(self) {
        unsafe { self.heap.as_ref().Release() };
    }
}

// `mapped_ptr` is safe to send or share across threads because
// it is never exposed publicly through [`MemoryBlock`].
unsafe impl Send for MemoryBlock {}
unsafe impl Sync for MemoryBlock {}

#[cfg(windows)]
struct MemoryType {
    memory_blocks: Vec<Option<MemoryBlock>>,
    memory_location: MemoryLocation,
    heap_category: HeapCategory,
    heap_properties: d3d12::D3D12_HEAP_PROPERTIES,
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
        backtrace: Option<&str>,
    ) -> Result<Allocation> {
        let allocation_type = AllocationType::Linear;

        let memblock_size = if self.heap_properties.Type == d3d12::D3D12_HEAP_TYPE_DEFAULT {
            DEFAULT_DEVICE_MEMBLOCK_SIZE
        } else {
            DEFAULT_HOST_MEMBLOCK_SIZE
        };

        let size = desc.size;
        let alignment = desc.alignment;

        // Create a dedicated block for large memory allocations
        if size > memblock_size {
            let mem_block = MemoryBlock::new(
                device,
                size,
                &self.heap_properties,
                self.heap_category,
                true,
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
                1,
                desc.name,
                backtrace,
            )?;

            return Ok(Allocation {
                chunk_id: Some(chunk_id),
                size,
                offset,
                memory_block_index: block_index,
                memory_type_index: self.memory_type_index as usize,
                heap: mem_block.heap.as_ptr(),
                name: Some(desc.name.to_owned()),
                backtrace: backtrace.map(|s| s.to_owned()),
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
                            heap: mem_block.heap.as_ptr(),
                            name: Some(desc.name.to_owned()),
                            backtrace: backtrace.map(|s| s.to_owned()),
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
            &self.heap_properties,
            self.heap_category,
            false,
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
            .ok_or_else(|| AllocationError::Internal("memory block must be Some".into()))?;
        let allocation = mem_block.sub_allocator.allocate(
            size,
            alignment,
            allocation_type,
            1,
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
            heap: mem_block.heap.as_ptr(),
            name: Some(desc.name.to_owned()),
            backtrace: backtrace.map(|s| s.to_owned()),
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
    device: std::ptr::NonNull<d3d12::ID3D12Device>,
    debug_settings: AllocatorDebugSettings,
    memory_types: Vec<MemoryType>,
}

impl Allocator {
    pub fn device(&self) -> &d3d12::ID3D12Device {
        unsafe { self.device.as_ref() }
    }

    pub fn new(desc: &AllocatorCreateDesc) -> Result<Self> {
        let device = std::ptr::NonNull::new(desc.device).ok_or_else(|| {
            AllocationError::InvalidAllocatorCreateDesc("Device pointer is null.".into())
        })?;

        // Query device for feature level
        let mut options = d3d12::D3D12_FEATURE_DATA_D3D12_OPTIONS::default();
        let hr = unsafe {
            device.as_ref().CheckFeatureSupport(
                d3d12::D3D12_FEATURE_D3D12_OPTIONS,
                &mut options as *mut _ as *mut _,
                std::mem::size_of_val(&options) as u32,
            )
        };
        if hr != winerror::S_OK {
            return Err(AllocationError::Internal(format!(
                "ID3D12Device::CheckFeatureSupport failed: {:x}",
                hr
            )));
        }

        let is_heap_tier1 = options.ResourceHeapTier == d3d12::D3D12_RESOURCE_HEAP_TIER_1;

        let heap_types = vec![
            (
                MemoryLocation::GpuOnly,
                d3d12::D3D12_HEAP_PROPERTIES {
                    Type: d3d12::D3D12_HEAP_TYPE_DEFAULT,
                    ..Default::default()
                },
            ),
            (
                MemoryLocation::CpuToGpu,
                d3d12::D3D12_HEAP_PROPERTIES {
                    Type: d3d12::D3D12_HEAP_TYPE_CUSTOM,
                    CPUPageProperty: d3d12::D3D12_CPU_PAGE_PROPERTY_WRITE_COMBINE,
                    MemoryPoolPreference: d3d12::D3D12_MEMORY_POOL_L0,
                    ..Default::default()
                },
            ),
            (
                MemoryLocation::GpuToCpu,
                d3d12::D3D12_HEAP_PROPERTIES {
                    Type: d3d12::D3D12_HEAP_TYPE_CUSTOM,
                    CPUPageProperty: d3d12::D3D12_CPU_PAGE_PROPERTY_WRITE_BACK,
                    MemoryPoolPreference: d3d12::D3D12_MEMORY_POOL_L0,
                    ..Default::default()
                },
            ),
        ];

        let heap_types = if is_heap_tier1 {
            heap_types
                .iter()
                .flat_map(|(memory_location, heap_properties)| {
                    [
                        (HeapCategory::Buffer, *memory_location, *heap_properties),
                        (
                            HeapCategory::RtvDsvTexture,
                            *memory_location,
                            *heap_properties,
                        ),
                        (
                            HeapCategory::OtherTexture,
                            *memory_location,
                            *heap_properties,
                        ),
                    ]
                    .to_vec()
                })
                .collect::<Vec<_>>()
        } else {
            heap_types
                .iter()
                .map(|(memory_location, heap_properties)| {
                    (HeapCategory::All, *memory_location, *heap_properties)
                })
                .collect::<Vec<_>>()
        };

        let memory_types = heap_types
            .iter()
            .enumerate()
            .map(
                |(i, &(heap_category, memory_location, heap_properties))| MemoryType {
                    memory_blocks: Vec::default(),
                    memory_location,
                    heap_category,
                    heap_properties,
                    memory_type_index: i,
                    active_general_blocks: 0,
                },
            )
            .collect::<Vec<_>>();

        Ok(Self {
            memory_types,
            device,
            debug_settings: desc.debug_settings,
        })
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
                "Allocating `{}` of {} bytes with an alignment of {}.",
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

        // Find memory type
        let memory_type = self
            .memory_types
            .iter_mut()
            .find(|memory_type| {
                let is_location_compatible = desc.location == MemoryLocation::Unknown
                    || desc.location == memory_type.memory_location;

                let is_category_compatible = memory_type.heap_category == HeapCategory::All
                    || memory_type.heap_category == desc.resource_category.into();

                is_location_compatible && is_category_compatible
            })
            .ok_or(AllocationError::NoCompatibleMemoryTypeFound)?;

        memory_type.allocate(unsafe { self.device.as_mut() }, desc, backtrace.as_deref())
    }

    pub fn free(&mut self, allocation: Allocation) -> Result<()> {
        if self.debug_settings.log_frees {
            let name = allocation.name.as_deref().unwrap_or("<null>");
            log!(Level::Debug, "Freeing `{}`.", name);
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
