#[cfg(feature = "std")]
use alloc::sync::Arc;
use alloc::{boxed::Box, string::ToString, vec::Vec};
use core::{fmt, marker::PhantomData};
#[cfg(feature = "std")]
use std::backtrace::Backtrace;

use ash::vk;
use log::{debug, Level};

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

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum AllocationScheme {
    /// Perform a dedicated, driver-managed allocation for the given buffer, allowing
    /// it to perform optimizations on this type of allocation.
    DedicatedBuffer(vk::Buffer),
    /// Perform a dedicated, driver-managed allocation for the given image, allowing
    /// it to perform optimizations on this type of allocation.
    DedicatedImage(vk::Image),
    /// The memory for this resource will be allocated and managed by gpu-allocator.
    GpuAllocatorManaged,
}

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
    /// Determines how this allocation should be managed.
    pub allocation_scheme: AllocationScheme,
}

/// Wrapper type to only mark a raw pointer [`Send`] + [`Sync`] without having to
/// mark the entire [`Allocation`] as such, instead relying on the compiler to
/// auto-implement this or fail if fields are added that violate this constraint
#[derive(Clone, Copy, Debug)]
pub(crate) struct SendSyncPtr(core::ptr::NonNull<core::ffi::c_void>);
// Sending is fine because mapped_ptr does not change based on the thread we are in
unsafe impl Send for SendSyncPtr {}
// Sync is also okay because Sending &Allocation is safe: a mutable reference
// to the data in mapped_ptr is never exposed while `self` is immutably borrowed.
// In order to break safety guarantees, the user needs to `unsafe`ly dereference
// `mapped_ptr` themselves.
unsafe impl Sync for SendSyncPtr {}

pub struct AllocatorCreateDesc {
    pub instance: ash::Instance,
    pub device: ash::Device,
    pub physical_device: vk::PhysicalDevice,
    pub debug_settings: AllocatorDebugSettings,
    pub buffer_device_address: bool,
    pub allocation_sizes: AllocationSizes,
}

/// A piece of allocated memory.
///
/// Could be contained in its own individual underlying memory object or as a sub-region
/// of a larger allocation.
///
/// # Copying data into a CPU-mapped [`Allocation`]
///
/// You'll very likely want to copy data into CPU-mapped [`Allocation`]s in order to send that data to the GPU.
/// Doing this data transfer correctly without invoking undefined behavior can be quite fraught and non-obvious<sup>[\[1\]]</sup>.
///
/// To help you do this correctly, [`Allocation`] implements [`presser::Slab`], which means you can directly
/// pass it in to many of `presser`'s [helper functions] (for example, [`copy_from_slice_to_offset`]).
///
/// In most cases, this will work perfectly. However, note that if you try to use an [`Allocation`] as a
/// [`Slab`] and it is not valid to do so (if it is not CPU-mapped or if its `size > isize::MAX`),
/// you will cause a panic. If you aren't sure about these conditions, you may use [`Allocation::try_as_mapped_slab`].
///
/// ## Example
///
/// Say we've created an [`Allocation`] called `my_allocation`, which is CPU-mapped.
/// ```ignore
/// let mut my_allocation: Allocation = my_allocator.allocate(...)?;
/// ```
///
/// And we want to fill it with some data in the form of a `my_gpu_data: Vec<MyGpuVector>`, defined as such:
///
/// ```ignore
/// // note that this is size(12) but align(16), thus we have 4 padding bytes.
/// // this would mean a `&[MyGpuVector]` is invalid to cast as a `&[u8]`, but
/// // we can still use `presser` to copy it directly in a valid manner.
/// #[repr(C, align(16))]
/// #[derive(Clone, Copy)]
/// struct MyGpuVertex {
///     x: f32,
///     y: f32,
///     z: f32,
/// }
///
/// let my_gpu_data: Vec<MyGpuData> = make_vertex_data();
/// ```
///
/// Depending on how the data we're copying will be used, the Vulkan device may have a minimum
/// alignment requirement for that data:
///
/// ```ignore
/// let min_gpu_align = my_vulkan_device_specifications.min_alignment_thing;
/// ```
///
/// Finally, we can use [`presser::copy_from_slice_to_offset_with_align`] to perform the copy,
/// simply passing `&mut my_allocation` since [`Allocation`] implements [`Slab`].
///
/// ```ignore
/// let copy_record = presser::copy_from_slice_to_offset_with_align(
///     &my_gpu_data[..], // a slice containing all elements of my_gpu_data
///     &mut my_allocation, // our Allocation
///     0, // start as close to the beginning of the allocation as possible
///     min_gpu_align, // the minimum alignment we queried previously
/// )?;
/// ```
///
/// It's important to note that the data may not have actually been copied starting at the requested
/// `start_offset` (0 in the example above) depending on the alignment of the underlying allocation
/// as well as the alignment requirements of `MyGpuVector` and the `min_gpu_align` we passed in. Thus,
/// we can query the `copy_record` for the actual starting offset:
///
/// ```ignore
/// let actual_data_start_offset = copy_record.copy_start_offset;
/// ```
///
/// ## Safety
///
/// It is technically not fully safe to use an [`Allocation`] as a [`presser::Slab`] because we can't validate that the
/// GPU is not using the data in the buffer while `self` is borrowed. However, trying
/// to validate this statically is really hard and the community has basically decided that
/// requiring `unsafe` for functions like this creates too much "unsafe-noise", ultimately making it
/// harder to debug more insidious unsafety that is unrelated to GPU-CPU sync issues.
///
/// So, as would always be the case, you must ensure the GPU
/// is not using the data in `self` for the duration that you hold the returned [`MappedAllocationSlab`].
///
/// [`Slab`]: presser::Slab
/// [`copy_from_slice_to_offset`]: presser::copy_from_slice_to_offset
/// [helper functions]: presser#functions
/// [\[1\]]: presser#motivation
#[derive(Debug)]
pub struct Allocation {
    chunk_id: Option<core::num::NonZeroU64>,
    offset: u64,
    size: u64,
    memory_block_index: usize,
    memory_type_index: usize,
    device_memory: vk::DeviceMemory,
    mapped_ptr: Option<SendSyncPtr>,
    dedicated_allocation: bool,
    memory_properties: vk::MemoryPropertyFlags,
    name: Option<Box<str>>,
}

impl Allocation {
    /// Tries to borrow the CPU-mapped memory that backs this allocation as a [`presser::Slab`], which you can then
    /// use to safely copy data into the raw, potentially-uninitialized buffer.
    /// See [the documentation of Allocation][Allocation#example] for an example of this.
    ///
    /// Returns [`None`] if `self.mapped_ptr()` is `None`, or if `self.size()` is greater than `isize::MAX` because
    /// this could lead to undefined behavior.
    ///
    /// Note that [`Allocation`] implements [`Slab`] natively, so you can actually pass this allocation as a [`Slab`]
    /// directly. However, if `self` is not actually a valid [`Slab`] (this function would return `None` as described above),
    /// then trying to use it as a [`Slab`] will panic.
    ///
    /// # Safety
    ///
    /// See the note about safety in [the documentation of Allocation][Allocation#safety]
    ///
    /// [`Slab`]: presser::Slab
    // best to be explicit where the lifetime is coming from since we're doing unsafe things
    // and relying on an inferred lifetime type in the PhantomData below
    #[allow(clippy::needless_lifetimes)]
    pub fn try_as_mapped_slab<'a>(&'a mut self) -> Option<MappedAllocationSlab<'a>> {
        let mapped_ptr = self.mapped_ptr()?.cast().as_ptr();

        if self.size > isize::MAX as _ {
            return None;
        }

        // this will always succeed since size is <= isize::MAX which is < usize::MAX
        let size = self.size as usize;

        Some(MappedAllocationSlab {
            _borrowed_alloc: PhantomData,
            mapped_ptr,
            size,
        })
    }

    pub fn chunk_id(&self) -> Option<core::num::NonZeroU64> {
        self.chunk_id
    }

    ///Returns the [`vk::MemoryPropertyFlags`] of this allocation.
    pub fn memory_properties(&self) -> vk::MemoryPropertyFlags {
        self.memory_properties
    }

    /// Returns the [`vk::DeviceMemory`] object that is backing this allocation.
    ///
    /// This memory object can be shared with multiple other allocations and shouldn't be freed or allocated from
    /// without this library, because that will lead to undefined behavior.
    ///
    /// # Safety
    /// The result of this function can safely be passed into [`ash::Device::bind_buffer_memory()`],
    /// [`ash::Device::bind_image_memory()`] etc. It is exposed for this reason.  Keep in mind to
    /// also pass [`Self::offset()`] along to those.
    ///
    /// Also, this [`Allocation`] must not be [`Allocator::free()`]d while such a created resource
    /// on this [`vk::DeviceMemory`] is still live.
    pub unsafe fn memory(&self) -> vk::DeviceMemory {
        self.device_memory
    }

    /// Returns [`true`] if this allocation is using a dedicated underlying allocation.
    pub fn is_dedicated(&self) -> bool {
        self.dedicated_allocation
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
    pub fn mapped_ptr(&self) -> Option<core::ptr::NonNull<core::ffi::c_void>> {
        self.mapped_ptr.map(|SendSyncPtr(p)| p)
    }

    /// Returns a valid mapped slice if the memory is host visible, otherwise it will return None.
    /// The slice already references the exact memory region of the allocation, so no offset needs to be applied.
    pub fn mapped_slice(&self) -> Option<&[u8]> {
        self.mapped_ptr().map(|ptr| unsafe {
            core::slice::from_raw_parts(ptr.cast().as_ptr(), self.size as usize)
        })
    }

    /// Returns a valid mapped mutable slice if the memory is host visible, otherwise it will return None.
    /// The slice already references the exact memory region of the allocation, so no offset needs to be applied.
    pub fn mapped_slice_mut(&mut self) -> Option<&mut [u8]> {
        self.mapped_ptr().map(|ptr| unsafe {
            core::slice::from_raw_parts_mut(ptr.cast().as_ptr(), self.size as usize)
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
            memory_properties: vk::MemoryPropertyFlags::empty(),
            name: None,
            dedicated_allocation: false,
        }
    }
}

/// A wrapper struct over a borrowed [`Allocation`] that infallibly implements [`presser::Slab`].
///
/// This type should be acquired by calling [`Allocation::try_as_mapped_slab`].
pub struct MappedAllocationSlab<'a> {
    _borrowed_alloc: PhantomData<&'a mut Allocation>,
    mapped_ptr: *mut u8,
    size: usize,
}

// SAFETY: See the safety comment of Allocation::as_mapped_slab above.
unsafe impl presser::Slab for MappedAllocationSlab<'_> {
    fn base_ptr(&self) -> *const u8 {
        self.mapped_ptr
    }

    fn base_ptr_mut(&mut self) -> *mut u8 {
        self.mapped_ptr
    }

    fn size(&self) -> usize {
        self.size
    }
}

// SAFETY: See the safety comment of Allocation::as_mapped_slab above.
unsafe impl presser::Slab for Allocation {
    fn base_ptr(&self) -> *const u8 {
        self.mapped_ptr
            .expect("tried to use a non-mapped Allocation as a Slab")
            .0
            .as_ptr()
            .cast()
    }

    fn base_ptr_mut(&mut self) -> *mut u8 {
        self.mapped_ptr
            .expect("tried to use a non-mapped Allocation as a Slab")
            .0
            .as_ptr()
            .cast()
    }

    fn size(&self) -> usize {
        if self.size > isize::MAX as _ {
            panic!("tried to use an Allocation with size > isize::MAX as a Slab")
        }
        // this will always work if the above passed
        self.size as usize
    }
}

#[derive(Debug)]
pub(crate) struct MemoryBlock {
    pub(crate) device_memory: vk::DeviceMemory,
    pub(crate) size: u64,
    pub(crate) mapped_ptr: Option<SendSyncPtr>,
    pub(crate) sub_allocator: Box<dyn SubAllocator>,
    #[cfg(feature = "visualizer")]
    pub(crate) dedicated_allocation: bool,
}

impl MemoryBlock {
    fn allocation_error(error: vk::Result) -> AllocationFailure {
        let public = match error {
            vk::Result::ERROR_OUT_OF_HOST_MEMORY | vk::Result::ERROR_OUT_OF_DEVICE_MEMORY => {
                AllocationError::OutOfMemory
            }
            other => AllocationError::Internal(format!(
                "Unexpected error in vkAllocateMemory: {other:?}"
            )),
        };
        if error == vk::Result::ERROR_TOO_MANY_OBJECTS {
            AllocationFailure::allocation_limit(public)
        } else {
            public.into()
        }
    }

    fn new(
        device: &ash::Device,
        size: u64,
        mem_type_index: usize,
        mapped: bool,
        buffer_device_address: bool,
        allocation_scheme: AllocationScheme,
        requires_personal_block: bool,
    ) -> AllocationResult<Self> {
        let device_memory = {
            let alloc_info = vk::MemoryAllocateInfo::default()
                .allocation_size(size)
                .memory_type_index(mem_type_index as u32);

            let allocation_flags = vk::MemoryAllocateFlags::DEVICE_ADDRESS;
            let mut flags_info = vk::MemoryAllocateFlagsInfo::default().flags(allocation_flags);
            // TODO(manon): Test this based on if the device has this feature enabled or not
            let alloc_info = if buffer_device_address {
                alloc_info.push_next(&mut flags_info)
            } else {
                alloc_info
            };

            // Flag the memory as dedicated if required.
            let mut dedicated_memory_info = vk::MemoryDedicatedAllocateInfo::default();
            let alloc_info = match allocation_scheme {
                AllocationScheme::DedicatedBuffer(buffer) => {
                    dedicated_memory_info = dedicated_memory_info.buffer(buffer);
                    alloc_info.push_next(&mut dedicated_memory_info)
                }
                AllocationScheme::DedicatedImage(image) => {
                    dedicated_memory_info = dedicated_memory_info.image(image);
                    alloc_info.push_next(&mut dedicated_memory_info)
                }
                AllocationScheme::GpuAllocatorManaged => alloc_info,
            };

            unsafe { device.allocate_memory(&alloc_info, None) }.map_err(Self::allocation_error)?
        };

        let mapped_ptr = if mapped {
            let pointer = match unsafe {
                device.map_memory(
                    device_memory,
                    0,
                    vk::WHOLE_SIZE,
                    vk::MemoryMapFlags::empty(),
                )
            } {
                Ok(pointer) => pointer,
                Err(error) => {
                    unsafe { device.free_memory(device_memory, None) };
                    return Err(match error {
                        vk::Result::ERROR_OUT_OF_HOST_MEMORY
                        | vk::Result::ERROR_OUT_OF_DEVICE_MEMORY => AllocationError::OutOfMemory,
                        vk::Result::ERROR_MEMORY_MAP_FAILED => {
                            AllocationError::FailedToMap(error.to_string())
                        }
                        other => AllocationError::Internal(format!(
                            "Unexpected error in vkMapMemory: {other:?}"
                        )),
                    }
                    .into());
                }
            };
            match core::ptr::NonNull::new(pointer) {
                Some(pointer) => Some(SendSyncPtr(pointer)),
                None => {
                    unsafe {
                        device.unmap_memory(device_memory);
                        device.free_memory(device_memory, None);
                    }
                    return Err(AllocationError::Internal(
                        "vkMapMemory returned a null pointer".into(),
                    )
                    .into());
                }
            }
        } else {
            None
        };

        let sub_allocator: Box<dyn SubAllocator> = if allocation_scheme
            != AllocationScheme::GpuAllocatorManaged
            || requires_personal_block
        {
            Box::new(DedicatedBlockAllocator::new(size))
        } else {
            Box::new(FreeListAllocator::new(size))
        };

        Ok(Self {
            device_memory,
            size,
            mapped_ptr,
            sub_allocator,
            #[cfg(feature = "visualizer")]
            dedicated_allocation: allocation_scheme != AllocationScheme::GpuAllocatorManaged,
        })
    }

    fn destroy(self, device: &ash::Device) {
        if self.mapped_ptr.is_some() {
            unsafe { device.unmap_memory(self.device_memory) };
        }

        unsafe { device.free_memory(self.device_memory, None) };
    }
}

#[derive(Debug)]
pub(crate) struct MemoryType {
    pub(crate) memory_blocks: Vec<Option<MemoryBlock>>,
    pub(crate) memory_properties: vk::MemoryPropertyFlags,
    pub(crate) memory_type_index: usize,
    pub(crate) heap_index: usize,
    pub(crate) heap_size: u64,
    pub(crate) mappable: bool,
    pub(crate) active_general_blocks: usize,
    active_blocks: usize,
    pub(crate) empty_block_index: Option<usize>,
    pub(crate) block_sizes: MemoryBlockSizes,
    pub(crate) buffer_device_address: bool,
}

impl MemoryType {
    fn block_size(&self, desc: &AllocationCreateDesc<'_>) -> u64 {
        let size = desc.requirements.size;
        if desc.allocation_scheme != AllocationScheme::GpuAllocatorManaged
            || size > self.block_sizes.dedicated_threshold()
        {
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

    fn release_cached(&mut self, device: &ash::Device) -> u64 {
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
            self.active_blocks -= 1;
            block.destroy(device);
            bytes
        } else {
            0
        }
    }

    fn allocate(
        &mut self,
        device: &ash::Device,
        desc: &AllocationCreateDesc<'_>,
        granularity: u64,
        #[cfg(feature = "std")] backtrace: Arc<Backtrace>,
        new_block_size: Option<u64>,
        allocation_limit_reached: bool,
    ) -> AllocationResult<Allocation> {
        let size = desc.requirements.size;
        let alignment = desc.requirements.alignment;
        if size > self.heap_size {
            return Err(AllocationError::OutOfMemory.into());
        }
        let allocation_type = if desc.linear {
            AllocationType::Linear
        } else {
            AllocationType::NonLinear
        };
        let dedicated_allocation = desc.allocation_scheme != AllocationScheme::GpuAllocatorManaged;
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
                    granularity,
                    desc.name,
                    #[cfg(feature = "std")]
                    backtrace.clone(),
                ) {
                    Ok((offset, chunk_id)) => {
                        if self.empty_block_index == Some(index) {
                            self.empty_block_index = None;
                        }
                        let mapped_ptr = block.mapped_ptr.and_then(|SendSyncPtr(ptr)| {
                            core::ptr::NonNull::new(unsafe { ptr.as_ptr().add(offset as usize) })
                                .map(SendSyncPtr)
                        });
                        return Ok(Allocation {
                            chunk_id: Some(chunk_id),
                            size,
                            offset,
                            memory_block_index: index,
                            memory_type_index: self.memory_type_index,
                            name: Some(desc.name.into()),
                            device_memory: block.device_memory,
                            mapped_ptr,
                            memory_properties: self.memory_properties,
                            dedicated_allocation: false,
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
        if allocation_limit_reached {
            return Err(AllocationFailure::allocation_limit(
                AllocationError::Internal("Allocator reached maxMemoryAllocationCount".into()),
            ));
        }
        let mut block = MemoryBlock::new(
            device,
            if dedicated { size } else { block_size },
            self.memory_type_index,
            self.mappable,
            self.buffer_device_address,
            desc.allocation_scheme,
            dedicated,
        )?;
        let (offset, chunk_id) = match block.sub_allocator.allocate(
            size,
            alignment,
            allocation_type,
            granularity,
            desc.name,
            #[cfg(feature = "std")]
            backtrace,
        ) {
            Ok(value) => value,
            Err(error) => {
                block.destroy(device);
                return Err(AllocationFailure::fatal(error));
            }
        };

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
        self.active_blocks += 1;
        if !dedicated {
            self.active_general_blocks += 1;
        }
        let block = self.memory_blocks[index]
            .as_ref()
            .ok_or_else(|| AllocationError::Internal("Published memory block is missing".into()))?;
        let mapped_ptr = block.mapped_ptr.and_then(|SendSyncPtr(ptr)| {
            core::ptr::NonNull::new(unsafe { ptr.as_ptr().add(offset as usize) }).map(SendSyncPtr)
        });
        Ok(Allocation {
            chunk_id: Some(chunk_id),
            size,
            offset,
            memory_block_index: index,
            memory_type_index: self.memory_type_index,
            name: Some(desc.name.into()),
            device_memory: block.device_memory,
            mapped_ptr,
            memory_properties: self.memory_properties,
            dedicated_allocation,
        })
    }

    #[allow(clippy::needless_pass_by_value)]
    fn free(&mut self, allocation: Allocation, device: &ash::Device) -> Result<()> {
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

            self.active_blocks -= 1;
            block.destroy(device);
        }

        Ok(())
    }
}

pub struct Allocator {
    pub(crate) memory_types: Vec<MemoryType>,
    pub(crate) memory_heaps: Vec<vk::MemoryHeap>,
    device: ash::Device,
    max_memory_allocation_count: usize,
    pub(crate) buffer_image_granularity: u64,
    pub(crate) debug_settings: AllocatorDebugSettings,
}

impl fmt::Debug for Allocator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.generate_report().fmt(f)
    }
}

impl Allocator {
    pub fn new(desc: &AllocatorCreateDesc) -> Result<Self> {
        if desc.physical_device == vk::PhysicalDevice::null() {
            return Err(AllocationError::InvalidAllocatorCreateDesc(
                "AllocatorCreateDesc field `physical_device` is null.".into(),
            ));
        }

        let mem_props = unsafe {
            desc.instance
                .get_physical_device_memory_properties(desc.physical_device)
        };

        let memory_types = &mem_props.memory_types_as_slice();
        let memory_heaps = mem_props.memory_heaps_as_slice().to_vec();

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

        let memory_types = memory_types
            .iter()
            .enumerate()
            .map(|(i, mem_type)| {
                let mappable = mem_type
                    .property_flags
                    .contains(vk::MemoryPropertyFlags::HOST_VISIBLE);
                let heap_size = memory_heaps[mem_type.heap_index as usize].size;
                MemoryType {
                    memory_blocks: Vec::default(),
                    memory_properties: mem_type.property_flags,
                    memory_type_index: i,
                    heap_index: mem_type.heap_index as usize,
                    heap_size,
                    mappable,
                    active_general_blocks: 0,
                    active_blocks: 0,
                    empty_block_index: None,
                    block_sizes: desc
                        .allocation_sizes
                        .block_sizes_for(mappable)
                        .capped_to_heap(heap_size),
                    buffer_device_address: desc.buffer_device_address,
                }
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
            max_memory_allocation_count: physical_device_properties
                .limits
                .max_memory_allocation_count as usize,
            buffer_image_granularity: granularity,
            debug_settings: desc.debug_settings,
        })
    }

    fn allocate_type(
        &mut self,
        index: usize,
        desc: &AllocationCreateDesc<'_>,
        #[cfg(feature = "std")] backtrace: &Arc<Backtrace>,
        block_size: Option<u64>,
    ) -> AllocationResult<Allocation> {
        // Other users of this VkDevice remain responsible for their own allocations.
        let limit_reached = self
            .memory_types
            .iter()
            .map(|ty| ty.active_blocks)
            .sum::<usize>()
            >= self.max_memory_allocation_count;
        self.memory_types[index].allocate(
            &self.device,
            desc,
            self.buffer_image_granularity,
            #[cfg(feature = "std")]
            backtrace.clone(),
            block_size,
            limit_reached,
        )
    }

    pub fn allocate(&mut self, desc: &AllocationCreateDesc<'_>) -> Result<Allocation> {
        let size = desc.requirements.size;
        let alignment = desc.requirements.alignment;

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

        let required = match desc.location {
            MemoryLocation::GpuOnly => vk::MemoryPropertyFlags::DEVICE_LOCAL,
            MemoryLocation::CpuToGpu | MemoryLocation::GpuToCpu => {
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT
            }
            MemoryLocation::Unknown => vk::MemoryPropertyFlags::empty(),
        };
        let preferred = required
            | match desc.location {
                MemoryLocation::CpuToGpu => vk::MemoryPropertyFlags::DEVICE_LOCAL,
                MemoryLocation::GpuToCpu => vk::MemoryPropertyFlags::HOST_CACHED,
                _ => vk::MemoryPropertyFlags::empty(),
            };
        let mut indices = [0usize; vk::MAX_MEMORY_TYPES];
        let mut count = 0;
        let mut compatible = false;
        for prefer in [true, false] {
            for (index, ty) in self.memory_types.iter().enumerate() {
                if desc.requirements.memory_type_bits & (1 << index) == 0
                    || !ty.memory_properties.contains(required)
                    // No deviceCoherentMemory enablement input is available.
                    || ty.memory_properties.contains(vk::MemoryPropertyFlags::DEVICE_COHERENT_AMD)
                {
                    continue;
                }
                compatible = true;
                if self.memory_heaps[ty.heap_index].size < size
                    || ty.memory_properties.contains(preferred) != prefer
                {
                    continue;
                }
                indices[count] = index;
                count += 1;
            }
        }
        if count == 0 {
            return Err(if compatible {
                AllocationError::OutOfMemory
            } else {
                AllocationError::NoCompatibleMemoryTypeFound
            });
        }
        let first = indices[0];
        let mut attempted_size = self.memory_types[first].block_size(desc);
        let first_error = match self.allocate_type(
            first,
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
        // Preserve usable blocks in another compatible type before reclaiming caches.
        for &index in &indices[1..count] {
            match self.allocate_type(
                index,
                desc,
                #[cfg(feature = "std")]
                &backtrace,
                None,
            ) {
                Ok(allocation) => return Ok(allocation),
                Err(error) if matches!(&error.error, AllocationError::OutOfMemory) => {}
                Err(error) => return Err(error.error),
            }
        }
        let released = self.trim();
        if released == 0 && first_error.is_allocation_limit() {
            return Err(first_error.error);
        }
        for (position, &index) in indices[..count].iter().enumerate() {
            if position != 0 || released != 0 {
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
            while can_shrink && desc.allocation_scheme == AllocationScheme::GpuAllocatorManaged {
                let Some(next) = crate::smaller_block_size(attempted_size, size, 1) else {
                    break;
                };
                attempted_size = next;
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
        }
        Err(first_error.error)
    }

    /// Returns the allocator capacity held by cached empty blocks.
    pub fn cached_capacity(&self) -> u64 {
        self.memory_types.iter().map(MemoryType::cached_size).sum()
    }

    /// Releases cached empty blocks and returns bytes removed from allocator capacity.
    /// Live indices remain valid. This does not wait for GPU work or release external references.
    pub fn trim(&mut self) -> u64 {
        self.memory_types
            .iter_mut()
            .map(|ty| ty.release_cached(&self.device))
            .sum()
    }

    /// Releases largest cached blocks until their total capacity is at most `bytes`.
    /// The target applies to empty cached capacity, not live allocations or an OS memory budget.
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
            let released = self.memory_types[index].release_cached(&self.device);
            debug_assert_eq!(released, size);
            if released == 0 {
                break;
            }
            cached -= released;
        }
        before - cached
    }

    pub fn free(&mut self, allocation: Allocation) -> Result<()> {
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

        let index = allocation.memory_type_index;
        let before = self.memory_types[index].cached_size();
        self.memory_types[index].free(allocation, &self.device)?;
        if self.memory_types[index].cached_size() > before {
            let heap = self.memory_types[index].heap_index;
            let mut cached: u64 = self
                .memory_types
                .iter()
                .filter(|ty| ty.heap_index == heap)
                .map(MemoryType::cached_size)
                .sum();
            // Overallocation may be supported, but retaining more empty memory than
            // the entire physical heap serves no useful working-set purpose.
            while cached > self.memory_heaps[heap].size {
                let Some((index, size)) = self
                    .memory_types
                    .iter()
                    .enumerate()
                    .filter(|(_, ty)| ty.heap_index == heap)
                    .map(|(index, ty)| (index, ty.cached_size()))
                    .max_by_key(|(_, size)| *size)
                else {
                    break;
                };
                let released = self.memory_types[index].release_cached(&self.device);
                debug_assert_eq!(released, size);
                if released == 0 {
                    break;
                }
                cached -= released;
            }
        }

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

#[cfg(all(test, feature = "std"))]
mod capacity_tests;

#[cfg(all(test, feature = "std"))]
mod resource_contract_tests;
#[cfg(all(test, feature = "std"))]
mod test_support;
