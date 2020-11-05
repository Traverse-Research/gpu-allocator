#![deny(clippy::unwrap_used)]
use ash::version::{DeviceV1_0, InstanceV1_0, InstanceV1_1};
use ash::vk;
use log::{log, Level};
use std::cell::RefCell;

mod result;
pub use result::*;

mod dedicated_block_allocator;
use dedicated_block_allocator::DedicatedBlockAllocator;

mod free_list_allocator;
use free_list_allocator::FreeListAllocator;

#[derive(Clone, Debug)]
pub struct AllocationCreateDesc<'a> {
    pub requirements: vk::MemoryRequirements,
    pub location: MemoryLocation,
    pub is_linear_resource: bool,
    pub name: &'a str,
}

const LOG_MEMORY_INFORMATION: bool = false;
const LOG_LEAKS_ON_SHUTDOWN: bool = true;
const STORE_STACK_TRACES: bool = true;
const LOG_ALLOCATIONS: bool = false;
const LOG_FREES: bool = false;
const LOG_STACK_TRACES: bool = false; // When LOG_ALLOCATIONS or LOG_FREES is enabled, enabling this flag will also log stack traces.

trait SubAllocator: std::fmt::Debug {
    fn allocate(
        &mut self,
        size: u64,
        alignment: u64,
        allocation_type: AllocationType,
        granularity: u64,
        name: &str,
        backtrace: Option<&str>,
    ) -> Result<(u64, std::num::NonZeroU64)>;

    fn free(&mut self, sub_allocation: &SubAllocation) -> Result<()>;

    fn report_memory_leaks(
        &self,
        log_level: Level,
        memory_type_index: usize,
        memory_block_index: usize,
    );

    #[must_use]
    fn supports_general_allocations(&self) -> bool;
    #[must_use]
    fn size(&self) -> u64;
    #[must_use]
    fn allocated(&self) -> u64;

    /// Helper function: reports how much memory is available in this suballocator
    #[must_use]
    fn available_memory(&self) -> u64 {
        self.size() - self.allocated()
    }

    /// Helper function: reports if the suballocator is empty (meaning, having no allocations).
    #[must_use]
    fn is_empty(&self) -> bool {
        self.allocated() == 0
    }
}

#[derive(Clone, Debug)]
pub struct SubAllocation {
    chunk_id: Option<std::num::NonZeroU64>,
    memory_block_index: usize,
    memory_type_index: usize,
    device_memory: vk::DeviceMemory,
    offset: u64,
    mapped_ptr: *mut std::ffi::c_void,

    name: Option<String>,
    backtrace: Option<String>,
}

unsafe impl Send for SubAllocation {}

impl SubAllocation {
    pub fn memory(&self) -> vk::DeviceMemory {
        self.device_memory
    }

    pub fn offset(&self) -> u64 {
        self.offset
    }

    pub fn mapped_ptr(&self) -> *mut std::ffi::c_void {
        self.mapped_ptr
    }

    pub fn is_null(&self) -> bool {
        self.chunk_id.is_none()
    }
}

impl Default for SubAllocation {
    fn default() -> Self {
        Self {
            chunk_id: None,
            memory_block_index: !0,
            memory_type_index: !0,
            device_memory: vk::DeviceMemory::null(),
            offset: 0,
            mapped_ptr: std::ptr::null_mut(),
            name: None,
            backtrace: None,
        }
    }
}

#[derive(PartialOrd, PartialEq, Eq, Clone, Copy, Debug)]
enum AllocationType {
    Free = 0,
    Linear = 1,
    NonLinear = 2,
}

#[derive(Debug)]
struct MemoryBlock {
    device_memory: vk::DeviceMemory,
    size: u64,
    mapped_ptr: *mut std::ffi::c_void,
    sub_allocator: Box<dyn SubAllocator>,
}

impl MemoryBlock {
    fn new(
        device: &ash::Device,
        size: u64,
        mem_type_index: usize,
        mapped: bool,
        dedicated: bool,
    ) -> Result<Self> {
        let device_memory = {
            let alloc_info = vk::MemoryAllocateInfo::builder()
                .allocation_size(size)
                .memory_type_index(mem_type_index as u32);

            let allocation_flags = vk::MemoryAllocateFlags::DEVICE_ADDRESS;
            let mut flags_info = vk::MemoryAllocateFlagsInfo::builder().flags(allocation_flags);
            // TODO(max): Test this based on if the device has this feature enabled or not
            let alloc_info = if cfg!(feature = "vulkan_device_address") {
                alloc_info.push_next(&mut flags_info)
            } else {
                alloc_info
            };

            unsafe { device.allocate_memory(&alloc_info, None) }
                .map_err(|_| AllocationError::OutOfMemory)?
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
            .map_err(|_| {
                unsafe { device.free_memory(device_memory, None) };
                AllocationError::FailedToMap
            })?
        } else {
            std::ptr::null_mut()
        };

        let sub_allocator: Box<dyn SubAllocator> = if dedicated {
            Box::new(DedicatedBlockAllocator::new(size))
        } else {
            Box::new(FreeListAllocator::new(size))
        };

        Ok(Self {
            device_memory,
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

#[derive(Debug)]
struct MemoryType {
    memory_blocks: Vec<Option<MemoryBlock>>,
    memory_properties: vk::MemoryPropertyFlags,
    memory_type_index: usize,
    heap_index: usize,
    mappable: bool,
    active_general_blocks: usize,
}

const DEFAULT_DEVICE_MEMBLOCK_SIZE: u64 = 256 * 1024 * 1024;
const DEFAULT_HOST_MEMBLOCK_SIZE: u64 = 64 * 1024 * 1024;

impl MemoryType {
    fn allocate(
        &mut self,
        device: &ash::Device,
        desc: &AllocationCreateDesc,
        granularity: u64,
        backtrace: Option<&str>,
    ) -> Result<SubAllocation> {
        let allocation_type = if desc.is_linear_resource {
            AllocationType::Linear
        } else {
            AllocationType::NonLinear
        };

        let memblock_size =
            if !(self.memory_properties & vk::MemoryPropertyFlags::HOST_VISIBLE).is_empty() {
                DEFAULT_HOST_MEMBLOCK_SIZE
            } else {
                DEFAULT_DEVICE_MEMBLOCK_SIZE
            };

        let size = desc.requirements.size;
        let alignment = desc.requirements.alignment;

        // Create a dedicated block for large memory allocations
        if size > memblock_size {
            let mem_block =
                MemoryBlock::new(device, size, self.memory_type_index, self.mappable, true)?;

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

            return Ok(SubAllocation {
                chunk_id: Some(chunk_id),
                memory_block_index: block_index,
                memory_type_index: self.memory_type_index as usize,
                device_memory: mem_block.device_memory,
                offset,
                mapped_ptr: mem_block.mapped_ptr,
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
                    granularity,
                    desc.name,
                    backtrace,
                );

                match allocation {
                    Ok((offset, chunk_id)) => {
                        let mapped_ptr = if !mem_block.mapped_ptr.is_null() {
                            unsafe { mem_block.mapped_ptr.add(offset as usize) }
                        } else {
                            std::ptr::null_mut()
                        };
                        return Ok(SubAllocation {
                            chunk_id: Some(chunk_id),
                            memory_block_index: mem_block_i,
                            memory_type_index: self.memory_type_index as usize,
                            device_memory: mem_block.device_memory,
                            offset,
                            mapped_ptr,
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
            self.memory_type_index,
            self.mappable,
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
            unsafe { mem_block.mapped_ptr.add(offset as usize) }
        } else {
            std::ptr::null_mut()
        };

        Ok(SubAllocation {
            chunk_id: Some(chunk_id),
            memory_block_index: new_block_index,
            memory_type_index: self.memory_type_index as usize,
            device_memory: mem_block.device_memory,
            offset,
            mapped_ptr,
            name: Some(desc.name.to_owned()),
            backtrace: backtrace.map(|s| s.to_owned()),
        })
    }

    fn free(&mut self, sub_allocation: &SubAllocation, device: &ash::Device) -> Result<()> {
        let mem_block = self.memory_blocks[sub_allocation.memory_block_index]
            .as_mut()
            .ok_or_else(|| AllocationError::Internal("Memory block must be Some.".into()))?;

        mem_block.sub_allocator.free(sub_allocation)?;

        if mem_block.sub_allocator.is_empty() {
            if mem_block.sub_allocator.supports_general_allocations() {
                if self.active_general_blocks > 1 {
                    let block = self.memory_blocks[sub_allocation.memory_block_index].take();
                    let block = block.ok_or_else(|| {
                        AllocationError::Internal("Memory block must be Some.".into())
                    })?;
                    block.destroy(device);

                    self.active_general_blocks -= 1;
                }
            } else {
                let block = self.memory_blocks[sub_allocation.memory_block_index].take();
                let block = block.ok_or_else(|| {
                    AllocationError::Internal("Memory block must be Some.".into())
                })?;
                block.destroy(device);
            }
        }

        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryLocation {
    Unknown,
    GpuOnly,
    CpuToGpu,
    GpuToCpu,
}

fn find_memorytype_index(
    memory_req: &vk::MemoryRequirements,
    memory_prop: &vk::PhysicalDeviceMemoryProperties,
    flags: vk::MemoryPropertyFlags,
) -> Option<u32> {
    memory_prop.memory_types[..memory_prop.memory_type_count as _]
        .iter()
        .enumerate()
        .find(|(index, memory_type)| {
            (1 << index) & memory_req.memory_type_bits != 0
                && memory_type.property_flags & flags == flags
        })
        .map(|(index, _memory_type)| index as _)
}

struct GpuAllocator {
    memory_types: Vec<MemoryType>,
    device: ash::Device,
    physical_mem_props: vk::PhysicalDeviceMemoryProperties,
    buffer_image_granularity: u64,
}

impl GpuAllocator {
    fn new(
        instance: &ash::Instance,
        device: &ash::Device,
        physical_device: ash::vk::PhysicalDevice,
    ) -> Result<Self> {
        let mem_props = unsafe { instance.get_physical_device_memory_properties(physical_device) };

        if LOG_MEMORY_INFORMATION {
            log!(
                Level::Debug,
                "memory type count: {}",
                mem_props.memory_type_count
            );
            log!(
                Level::Debug,
                "memory heap count: {}",
                mem_props.memory_heap_count
            );
            for i in 0..mem_props.memory_type_count {
                let mem_type = mem_props.memory_types[i as usize];
                let flags = mem_type.property_flags;
                log!(
                    Level::Debug,
                    "memory type[{}]: prop flags: 0x{:x}, heap[{}]",
                    i,
                    flags.as_raw(),
                    mem_type.heap_index,
                );
            }
            for i in 0..mem_props.memory_heap_count {
                log!(
                    Level::Debug,
                    "heap[{}] flags: 0x{:x}, size: {} MiB",
                    i,
                    mem_props.memory_heaps[i as usize].flags.as_raw(),
                    mem_props.memory_heaps[i as usize].size / (1024 * 1024)
                );
            }
        }

        let memory_types = (0..mem_props.memory_type_count)
            .map(|i| {
                let i = i as usize;

                let mem_type = &mem_props.memory_types[i];

                MemoryType {
                    memory_blocks: Vec::default(),
                    memory_properties: mem_type.property_flags,
                    memory_type_index: i,
                    heap_index: mem_props.memory_types[i].heap_index as usize,
                    mappable: (mem_type.property_flags & vk::MemoryPropertyFlags::HOST_VISIBLE)
                        != vk::MemoryPropertyFlags::empty(),
                    active_general_blocks: 0,
                }
            })
            .collect::<Vec<_>>();

        // NOTE(max): Test if there is any HOST_VISIBLE memory that does _not_
        //            have the HOST_COHERENT flag, in that case we want to panic,
        //            as we want to do cool things that we do not yet support
        //            with that type of memory :)
        for i in 0..mem_props.memory_type_count {
            let flags = mem_props.memory_types[i as usize].property_flags;

            if (flags & vk::MemoryPropertyFlags::HOST_VISIBLE) != vk::MemoryPropertyFlags::empty()
                && (flags & vk::MemoryPropertyFlags::HOST_COHERENT)
                    == vk::MemoryPropertyFlags::empty()
            {
                log!(Level::Warn, "There is a memory type that is host visible, but not host coherent. It's time to upgrade our memory allocator to take advantage of this type of memory :)");
            }
        }

        let mut physical_device_properties2 = vk::PhysicalDeviceProperties2::default();

        unsafe {
            instance
                .get_physical_device_properties2(physical_device, &mut physical_device_properties2)
        };

        let granularity = physical_device_properties2
            .properties
            .limits
            .buffer_image_granularity;

        Ok(Self {
            memory_types,
            device: device.clone(),
            physical_mem_props: mem_props,
            buffer_image_granularity: granularity,
        })
    }

    fn allocate(&mut self, desc: &AllocationCreateDesc) -> Result<SubAllocation> {
        let size = desc.requirements.size;
        let alignment = desc.requirements.alignment;

        let backtrace = if STORE_STACK_TRACES {
            Some(format!("{:?}", backtrace::Backtrace::new()))
        } else {
            None
        };

        if LOG_ALLOCATIONS {
            log!(
                Level::Debug,
                "Allocating \"{}\" of {} bytes with an alignment of {}.",
                &desc.name,
                size,
                alignment
            );
            if LOG_STACK_TRACES {
                let backtrace = backtrace
                    .clone()
                    .unwrap_or(format!("{:?}", backtrace::Backtrace::new()));
                log!(Level::Debug, "Allocation stack trace: {}", &backtrace);
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
        let mut memory_type_index_opt = find_memorytype_index(
            &desc.requirements,
            &self.physical_mem_props,
            mem_loc_preferred_bits,
        );

        if memory_type_index_opt.is_none() {
            let mem_loc_required_bits = match desc.location {
                MemoryLocation::GpuOnly => vk::MemoryPropertyFlags::DEVICE_LOCAL,
                MemoryLocation::CpuToGpu => {
                    vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT
                }
                MemoryLocation::GpuToCpu => {
                    vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT
                }
                MemoryLocation::Unknown => vk::MemoryPropertyFlags::empty(),
            };

            memory_type_index_opt = find_memorytype_index(
                &desc.requirements,
                &self.physical_mem_props,
                mem_loc_required_bits,
            );
        }

        let memory_type_index = match memory_type_index_opt {
            Some(x) => x as usize,
            None => return Err(AllocationError::NoCompatibleMemoryTypeFound),
        };

        let sub_allocation = self.memory_types[memory_type_index].allocate(
            &self.device,
            desc,
            self.buffer_image_granularity,
            backtrace.as_deref(),
        );

        if desc.location == MemoryLocation::CpuToGpu {
            if sub_allocation.is_err() {
                let mem_loc_preferred_bits =
                    vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;

                let memory_type_index_opt = find_memorytype_index(
                    &desc.requirements,
                    &self.physical_mem_props,
                    mem_loc_preferred_bits,
                );

                let memory_type_index = match memory_type_index_opt {
                    Some(x) => x as usize,
                    None => return Err(AllocationError::NoCompatibleMemoryTypeFound),
                };

                self.memory_types[memory_type_index].allocate(
                    &self.device,
                    desc,
                    self.buffer_image_granularity,
                    backtrace.as_deref(),
                )
            } else {
                sub_allocation
            }
        } else {
            sub_allocation
        }
    }

    fn free(&mut self, sub_allocation: &SubAllocation) -> Result<()> {
        if LOG_FREES {
            let name = sub_allocation.name.as_deref().unwrap_or("<null>");
            log!(Level::Debug, "Free'ing \"{}\".", name);
            if LOG_STACK_TRACES {
                let backtrace = format!("{:?}", backtrace::Backtrace::new());
                log!(Level::Debug, "Free stack trace: {}", backtrace);
            }
        }

        if sub_allocation.is_null() {
            return Ok(());
        }

        self.memory_types[sub_allocation.memory_type_index].free(sub_allocation, &self.device)?;

        Ok(())
    }

    fn report_memory_leaks(&self, log_level: Level) {
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

pub struct Allocator {
    allocator: RefCell<GpuAllocator>,
}

impl std::fmt::Debug for Allocator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GPU Memory Allocator")
    }
}

impl Allocator {
    /// Initializes a new memory allocator.
    pub fn new(
        instance: &ash::Instance,
        device: &ash::Device,
        physical_device: ash::vk::PhysicalDevice,
    ) -> Result<Self> {
        let allocator = GpuAllocator::new(instance, device, physical_device)?;

        Ok(Self {
            allocator: RefCell::new(allocator),
        })
    }

    /// Log all allocations that have not been free'd.
    pub fn report_memory_leaks(&self, log_level: Level) {
        self.allocator.borrow_mut().report_memory_leaks(log_level);
    }

    /// Attempt to allocate memory. Will return an error on failure.
    pub fn alloc(&self, desc: &AllocationCreateDesc) -> Result<SubAllocation> {
        self.allocator.borrow_mut().allocate(desc)
    }

    /// Free previously allocated memory. Only error that can occur is an internal error.
    pub fn free(&self, sub_alloc: &SubAllocation) -> Result<()> {
        self.allocator.borrow_mut().free(sub_alloc)?;
        Ok(())
    }
}

impl Drop for Allocator {
    fn drop(&mut self) {
        if LOG_LEAKS_ON_SHUTDOWN {
            self.report_memory_leaks(Level::Warn);
        }
    }
}
