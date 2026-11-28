//! This crate provides a fully written in Rust memory allocator for Vulkan, DirectX 12 and Metal.
//!
//! # Setting up the Vulkan memory allocator
//!
//! ```no_run
//! # #[cfg(feature = "vulkan")]
//! # fn main() {
//! use gpu_allocator::vulkan::*;
//! # use ash::vk;
//! # let device = todo!();
//! # let instance = todo!();
//! # let physical_device = todo!();
//!
//! let mut allocator = Allocator::new(&AllocatorCreateDesc {
//!     instance,
//!     device,
//!     physical_device,
//!     debug_settings: Default::default(),
//!     buffer_device_address: true,  // Ideally, check the BufferDeviceAddressFeatures struct.
//!     allocation_sizes: Default::default(),
//! });
//! # }
//! # #[cfg(not(feature = "vulkan"))]
//! # fn main() {}
//! ```
//!
//! # Simple Vulkan allocation example
//!
//! ```no_run
//! # #[cfg(feature = "vulkan")]
//! # fn main() {
//! use gpu_allocator::vulkan::*;
//! use gpu_allocator::MemoryLocation;
//! # use ash::vk;
//! # let device = todo!();
//! # let instance = todo!();
//! # let physical_device = todo!();
//! # let mut allocator = Allocator::new(&AllocatorCreateDesc {
//! #     instance,
//! #     device,
//! #     physical_device,
//! #     debug_settings: Default::default(),
//! #     buffer_device_address: true,  // Ideally, check the BufferDeviceAddressFeatures struct.
//! #     allocation_sizes: Default::default(),
//! # }).unwrap();
//!
//! // Setup vulkan info
//! let vk_info = vk::BufferCreateInfo::default()
//!     .size(512)
//!     .usage(vk::BufferUsageFlags::STORAGE_BUFFER);
//!
//! let buffer = unsafe { device.create_buffer(&vk_info, None) }.unwrap();
//! let requirements = unsafe { device.get_buffer_memory_requirements(buffer) };
//!
//! let allocation = allocator
//!     .allocate(&AllocationCreateDesc {
//!         name: "Example allocation",
//!         requirements,
//!         location: MemoryLocation::CpuToGpu,
//!         linear: true, // Buffers are always linear
//!         allocation_scheme: AllocationScheme::GpuAllocatorManaged,
//!     }).unwrap();
//!
//! // Bind memory to the buffer
//! unsafe { device.bind_buffer_memory(buffer, allocation.memory(), allocation.offset()).unwrap() };
//!
//! // Cleanup
//! allocator.free(allocation).unwrap();
//! unsafe { device.destroy_buffer(buffer, None) };
//! # }
//! # #[cfg(not(feature = "vulkan"))]
//! # fn main() {}
//! ```
//!
//! # Setting up the D3D12 memory allocator
//!
//! ```no_run
//! # #[cfg(feature = "d3d12")]
//! # fn main() {
//! use gpu_allocator::d3d12::*;
//! # let device = todo!();
//!
//! let mut allocator = Allocator::new(&AllocatorCreateDesc {
//!     device: ID3D12DeviceVersion::Device(device),
//!     debug_settings: Default::default(),
//!     allocation_sizes: Default::default(),
//! });
//! # }
//! # #[cfg(not(feature = "d3d12"))]
//! # fn main() {}
//! ```
//!
//! # Simple d3d12 allocation example
//!
//! ```no_run
//! # #[cfg(feature = "d3d12")]
//! # fn main() -> windows::core::Result<()> {
//! use gpu_allocator::d3d12::*;
//! use gpu_allocator::MemoryLocation;
//! # use windows::Win32::Graphics::{Dxgi, Direct3D12};
//! # let device = todo!();
//!
//! # let mut allocator = Allocator::new(&AllocatorCreateDesc {
//! #     device: ID3D12DeviceVersion::Device(device),
//! #     debug_settings: Default::default(),
//! #     allocation_sizes: Default::default(),
//! # }).unwrap();
//!
//! let buffer_desc = Direct3D12::D3D12_RESOURCE_DESC {
//!     Dimension: Direct3D12::D3D12_RESOURCE_DIMENSION_BUFFER,
//!     Alignment: 0,
//!     Width: 512,
//!     Height: 1,
//!     DepthOrArraySize: 1,
//!     MipLevels: 1,
//!     Format: Dxgi::Common::DXGI_FORMAT_UNKNOWN,
//!     SampleDesc: Dxgi::Common::DXGI_SAMPLE_DESC {
//!         Count: 1,
//!         Quality: 0,
//!     },
//!     Layout: Direct3D12::D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
//!     Flags: Direct3D12::D3D12_RESOURCE_FLAG_NONE,
//! };
//! let allocation_desc = AllocationCreateDesc::from_d3d12_resource_desc(
//!     &allocator.device(),
//!     &buffer_desc,
//!     "Example allocation",
//!     MemoryLocation::GpuOnly,
//! );
//! let allocation = allocator.allocate(&allocation_desc).unwrap();
//! let mut resource: Option<Direct3D12::ID3D12Resource> = None;
//! let hr = unsafe {
//!     device.CreatePlacedResource(
//!         allocation.heap(),
//!         allocation.offset(),
//!         &buffer_desc,
//!         Direct3D12::D3D12_RESOURCE_STATE_COMMON,
//!         None,
//!         &mut resource,
//!     )
//! }?;
//!
//! // Cleanup
//! drop(resource);
//! allocator.free(allocation).unwrap();
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "d3d12"))]
//! # fn main() {}
//! ```
//!
//! # Setting up the Metal memory allocator
//!
//! ```no_run
//! # #[cfg(feature = "metal")]
//! # fn main() {
//! use gpu_allocator::metal::*;
//! # let device = objc2_metal::MTLCreateSystemDefaultDevice().expect("No MTLDevice found");
//! let mut allocator = Allocator::new(&AllocatorCreateDesc {
//!     device: device.clone(),
//!     debug_settings: Default::default(),
//!     allocation_sizes: Default::default(),
//!     create_residency_set: false,
//! });
//! # }
//! # #[cfg(not(feature = "metal"))]
//! # fn main() {}
//! ```
//!
//! # Simple Metal allocation example
//!
//! ```no_run
//! # #[cfg(feature = "metal")]
//! # fn main() {
//! use gpu_allocator::metal::*;
//! use gpu_allocator::MemoryLocation;
//! # let device = objc2_metal::MTLCreateSystemDefaultDevice().expect("No MTLDevice found");
//! # let mut allocator = Allocator::new(&AllocatorCreateDesc {
//! #     device: device.clone(),
//! #     debug_settings: Default::default(),
//! #     allocation_sizes: Default::default(),
//! #    create_residency_set: false,
//! # })
//! # .unwrap();
//! let allocation_desc = AllocationCreateDesc::buffer(
//!     &device,
//!     "Example allocation",
//!     512, // size in bytes
//!     MemoryLocation::GpuOnly,
//! );
//! let allocation = allocator.allocate(&allocation_desc).unwrap();
//! # use objc2_metal::MTLHeap;
//! let heap = unsafe { allocation.heap() };
//! let resource = unsafe {
//!     heap.newBufferWithLength_options_offset(
//!         allocation.size() as usize,
//!         heap.resourceOptions(),
//!         allocation.offset() as usize,
//!     )
//! }
//! .unwrap();
//!
//! // Cleanup
//! drop(resource);
//! allocator.free(&allocation).unwrap();
//! # }
//! # #[cfg(not(feature = "metal"))]
//! # fn main() {}
//! ```
#![deny(clippy::unimplemented, clippy::unwrap_used, clippy::ok_expect)]
#![warn(
    clippy::alloc_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::std_instead_of_core
)]
#![cfg_attr(not(feature = "std"), no_std)]

#[macro_use]
extern crate alloc;

#[cfg(all(not(feature = "std"), feature = "visualizer"))]
compile_error!("Cannot enable `visualizer` feature in `no_std` environment.");

#[cfg(not(any(feature = "std", feature = "hashbrown")))]
compile_error!("Either `std` or `hashbrown` feature must be enabled");

mod result;
pub use result::*;

pub mod allocator;

pub use allocator::{
    AllocationReport, AllocatorReport, DedicatedBlockAllocator, FreeListAllocator,
    MemoryBlockReport,
};

#[cfg(feature = "visualizer")]
pub mod visualizer;

#[cfg(feature = "vulkan")]
pub mod vulkan;

#[cfg(all(windows, feature = "d3d12"))]
pub mod d3d12;

#[cfg(all(target_vendor = "apple", feature = "metal"))]
pub mod metal;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MemoryLocation {
    /// The allocated resource is stored at an unknown memory location; let the driver decide what's the best location
    Unknown,
    /// Store the allocation in GPU only accessible memory - typically this is the faster GPU resource and this should be
    /// where most of the allocations live.
    GpuOnly,
    /// Memory useful for uploading data to the GPU and potentially for constant buffers
    CpuToGpu,
    /// Memory useful for CPU readback of data
    GpuToCpu,
}

#[non_exhaustive]
#[derive(Copy, Clone, Debug)]
pub struct AllocatorDebugSettings {
    /// Logs out debugging information about the various heaps the current device has on startup
    pub log_memory_information: bool,
    /// Logs out all memory leaks on shutdown with log level Warn
    pub log_leaks_on_shutdown: bool,
    /// Stores a copy of the full backtrace for every allocation made, this makes it easier to debug leaks
    /// or other memory allocations, but storing stack traces has a RAM overhead so should be disabled
    /// in shipping applications.
    #[cfg(feature = "std")]
    pub store_stack_traces: bool,
    /// Log out every allocation as it's being made with log level Debug, rather spammy so off by default
    pub log_allocations: bool,
    /// Log out every free that is being called with log level Debug, rather spammy so off by default
    pub log_frees: bool,
    /// Log out stack traces when either `log_allocations` or `log_frees` is enabled.
    #[cfg(feature = "std")]
    pub log_stack_traces: bool,
}

impl Default for AllocatorDebugSettings {
    fn default() -> Self {
        Self {
            log_memory_information: false,
            log_leaks_on_shutdown: true,
            #[cfg(feature = "std")]
            store_stack_traces: false,
            log_allocations: false,
            log_frees: false,
            #[cfg(feature = "std")]
            log_stack_traces: false,
        }
    }
}

/// The sizes of the memory blocks that the allocator will create.
///
/// Useful for tuning the allocator to your application's needs. For example most games will be fine with the default
/// values, but eg. an app might want to use smaller block sizes to reduce the amount of memory used.
///
/// Configured sizes are clamped between 4MB and 256MB and rounded up to a multiple of 4MB.
/// Vulkan additionally limits shared block sizes to the capacity of the selected memory heap.
///
/// Note that these limits only apply to shared memory blocks that can hold multiple allocations.
/// If an allocation does not fit within the corresponding maximum block size, it will be placed
/// in a dedicated memory block holding only this allocation, without limitations other than what
/// the underlying hardware and driver are able to provide.
///
/// # Fixed or growable block size
///
/// This structure represents ranges of allowed sizes for shared memory blocks.
/// By default, if the upper bounds are not extended using `with_max_*_memblock_size`,
/// the allocator will be configured to use a fixed memory block size for shared
/// allocations.
///
/// Otherwise, the allocator will pick a memory block size within the specifed
/// range, depending on the number of existing allocations for the memory
/// type.
///
/// As a rule of thumb, the allocator will start with the minimum block size
/// and double the size with each new allocation, up to the specified maximum
/// block size. If a single allocation is larger than the current step size
/// (but still fits within the maximum block size), the new shared block size
/// advances geometrically to the smallest step that fits. This growth is
/// tracked independently for each memory type using occupied shared blocks. An empty
/// cached block does not advance growth. Under memory pressure, backends reclaim empty
/// caches and may allocate a smaller shared block, below the preferred minimum, to fit
/// the resource. The maximum remains the shared/dedicated allocation threshold.
/// Each memory type can cache one empty shared block, including while other blocks
/// remain in use. Its size is limited to the smaller of twice the minimum and the
/// maximum block size. Larger empty blocks are released. Backend `Allocator::trim`
/// methods release cached empty blocks at an application-selected boundary;
/// `cached_capacity` reports their aggregate capacity and `trim_to` bounds it.
/// Per-type limits add together when multiple types use the same physical heap.
///
/// # Example
///
/// ```
/// use gpu_allocator::AllocationSizes;
/// const MB: u64 = 1024 * 1024;
/// // This configuration uses fixed memory block sizes.
/// let fixed = AllocationSizes::new(256 * MB, 64 * MB);
///
/// // This configuration starts with 8MB memory blocks
/// // and grows the block size of a given memory type each
/// // time a new allocation is needed, up to a limit of
/// // 256MB for device memory and 64MB for host memory.
/// let growing = AllocationSizes::new(8 * MB, 8 * MB)
///     .with_max_device_memblock_size(256 * MB)
///     .with_max_host_memblock_size(64 * MB);
/// ```
#[derive(Clone, Copy, Debug)]
pub struct AllocationSizes {
    /// The initial shared block size for memory types the CPU cannot access.
    ///
    /// The size of new device memory blocks doubles each time a new block is needed, up to
    /// [`AllocationSizes::max_device_memblock_size`].
    ///
    /// Defaults to 256MB.
    min_device_memblock_size: u64,
    /// The maximum shared block size for memory types the CPU cannot access.
    ///
    /// Defaults to the value of [`AllocationSizes::min_device_memblock_size`].
    max_device_memblock_size: u64,
    /// The initial shared block size for CPU-visible memory types, including
    /// CPU-visible device-local memory on BAR, resizable BAR and unified-memory systems.
    ///
    /// The size of new host memory blocks doubles each time a new block is needed, up to
    /// [`AllocationSizes::max_host_memblock_size`].
    ///
    /// Defaults to 64MB.
    min_host_memblock_size: u64,
    /// The maximum shared block size for CPU-visible memory types.
    ///
    /// Defaults to the value of [`AllocationSizes::min_host_memblock_size`].
    max_host_memblock_size: u64,
}

impl AllocationSizes {
    /// Sets the minimum device and host memory block sizes.
    ///
    /// The maximum block sizes are initialized to the minimum sizes and
    /// can be increased using [`AllocationSizes::with_max_device_memblock_size`] and
    /// [`AllocationSizes::with_max_host_memblock_size`].
    pub fn new(device_memblock_size: u64, host_memblock_size: u64) -> Self {
        let device_memblock_size = Self::adjust_memblock_size(device_memblock_size, "Device");
        let host_memblock_size = Self::adjust_memblock_size(host_memblock_size, "Host");

        Self {
            min_device_memblock_size: device_memblock_size,
            max_device_memblock_size: device_memblock_size,
            min_host_memblock_size: host_memblock_size,
            max_host_memblock_size: host_memblock_size,
        }
    }

    /// Sets the maximum device memblock size, in bytes.
    pub fn with_max_device_memblock_size(mut self, size: u64) -> Self {
        self.max_device_memblock_size =
            Self::adjust_memblock_size(size, "Device").max(self.min_device_memblock_size);

        self
    }

    /// Sets the maximum host memblock size, in bytes.
    pub fn with_max_host_memblock_size(mut self, size: u64) -> Self {
        self.max_host_memblock_size =
            Self::adjust_memblock_size(size, "Host").max(self.min_host_memblock_size);

        self
    }

    fn adjust_memblock_size(size: u64, kind: &str) -> u64 {
        const MB: u64 = 1024 * 1024;

        let size = size.clamp(4 * MB, 256 * MB);

        if size % (4 * MB) == 0 {
            return size;
        }

        let val = size / (4 * MB) + 1;
        let new_size = val * 4 * MB;
        log::warn!(
            "{kind} memory block size must be a multiple of 4MB, clamping to {}MB",
            new_size / MB
        );

        new_size
    }

    /// Used internally to decide the size of a shared memory block
    /// based within the allowed range, based on the number of
    /// existing shared blocks. The more blocks there already are
    /// (where the requested allocation didn't fit), the larger
    /// the returned memory block size is going to be (up to
    /// `max_*_memblock_size`).
    ///
    /// `cpu_visible` selects the host range (memory types the CPU can map, including
    /// device-local CPU-visible memory such as BAR/ReBAR or unified memory) instead of the
    /// device range (memory types the CPU cannot access).
    /// This returns the configured growth step before backend hardware limits.
    pub fn get_memblock_size(&self, cpu_visible: bool, count: usize) -> u64 {
        self.block_sizes_for(cpu_visible).step_size(count)
    }

    /// Resolves the shared memory block size range that applies to one memory type.
    ///
    /// Each backend calls this exactly once per memory type, when its `Allocator` is
    /// created, so that allocation and deallocation always use the same range.
    pub(crate) fn block_sizes_for(&self, cpu_visible: bool) -> MemoryBlockSizes {
        if cpu_visible {
            MemoryBlockSizes {
                min: self.min_host_memblock_size,
                max: self.max_host_memblock_size,
            }
        } else {
            MemoryBlockSizes {
                min: self.min_device_memblock_size,
                max: self.max_device_memblock_size,
            }
        }
    }
}

/// The shared memory block size range of a single memory type, resolved from
/// [`AllocationSizes`] when the allocator is created.
///
/// All block sizing decisions for a memory type (step size, dedicated threshold, size of a new
/// shared block, empty block retention) are made from this one value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MemoryBlockSizes {
    pub(crate) min: u64,
    pub(crate) max: u64,
}

#[allow(dead_code)] // Not every backend uses every helper in every feature combination.
impl MemoryBlockSizes {
    /// Size of the next shared block when `active_general_blocks` shared blocks exist.
    pub(crate) fn step_size(&self, active_general_blocks: usize) -> u64 {
        // The ranges are clamped to 4MB..256MB so we never need to
        // shift by more than 7 bits. Clamping here to avoid having
        // to worry about overflows.
        let shift = active_general_blocks.min(7) as u64;
        (self.min << shift).min(self.max)
    }

    /// Hardware capacity is a hard limit, including on heaps smaller than 4 MiB.
    pub(crate) fn capped_to_heap(self, capacity: u64) -> Self {
        Self {
            min: self.min.min(capacity),
            max: self.max.min(capacity),
        }
    }

    /// Allocations larger than this are placed in a dedicated block.
    pub(crate) fn dedicated_threshold(&self) -> u64 {
        self.max
    }

    /// Size of a new shared block that must hold an allocation of `allocation_size` bytes
    /// (`allocation_size <= self.dedicated_threshold()`).
    ///
    /// Starts at [`Self::step_size`] and advances geometrically, up to the maximum, to the
    /// smallest step that fits the allocation.
    pub(crate) fn shared_block_size(
        &self,
        active_general_blocks: usize,
        allocation_size: u64,
    ) -> u64 {
        let mut block_size = self.step_size(active_general_blocks);
        while block_size < allocation_size && block_size < self.max {
            block_size = block_size.saturating_mul(2).min(self.max);
        }
        block_size
    }

    /// Keep the largest empty block within one growth step of the configured minimum.
    pub(crate) fn should_cache_empty_block(&self, size: u64, cached_size: Option<u64>) -> bool {
        size <= self.min.saturating_mul(2).min(self.max)
            && cached_size.map_or(true, |cached| size > cached)
    }
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12"), test))]
pub(crate) fn smaller_block_size(current: u64, required: u64, alignment: u64) -> Option<u64> {
    if current <= required {
        return None;
    }
    let next = (current / 2).max(required).checked_add(alignment - 1)? & !(alignment - 1);
    (next < current).then_some(next)
}

impl Default for AllocationSizes {
    fn default() -> Self {
        const MB: u64 = 1024 * 1024;
        Self {
            min_device_memblock_size: 256 * MB,
            max_device_memblock_size: 256 * MB,
            min_host_memblock_size: 64 * MB,
            max_host_memblock_size: 64 * MB,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AllocationSizes;

    const MB: u64 = 1024 * 1024;

    #[test]
    fn growable_memblock_size_advances_to_fit_allocation_up_to_max() {
        let sizes = AllocationSizes::new(8 * MB, 4 * MB)
            .with_max_device_memblock_size(256 * MB)
            .with_max_host_memblock_size(64 * MB);

        assert_eq!(sizes.block_sizes_for(false).dedicated_threshold(), 256 * MB);
        assert_eq!(sizes.block_sizes_for(true).dedicated_threshold(), 64 * MB);

        // Allocation smaller than current step keeps current step.
        assert_eq!(
            sizes.block_sizes_for(false).shared_block_size(0, MB),
            8 * MB
        );
        assert_eq!(
            sizes.block_sizes_for(false).shared_block_size(1, 16 * MB),
            16 * MB
        );

        // Allocation larger than current step (16 MiB) but <= max (256 MiB)
        // advances geometrically to the smallest step that fits.
        assert_eq!(
            sizes
                .block_sizes_for(false)
                .shared_block_size(1, 16 * MB + 65536),
            32 * MB
        );
        assert_eq!(
            sizes.block_sizes_for(false).shared_block_size(0, 100 * MB),
            128 * MB
        );
        assert_eq!(
            sizes.block_sizes_for(false).shared_block_size(0, 256 * MB),
            256 * MB
        );

        // Host range behaves identically with its own min/max bounds.
        assert_eq!(
            sizes.block_sizes_for(true).shared_block_size(0, 10 * MB),
            16 * MB
        );
        assert_eq!(
            sizes.block_sizes_for(true).shared_block_size(1, 50 * MB),
            64 * MB
        );
    }

    #[test]
    fn non_power_of_two_max_clamps_cleanly() {
        let sizes = AllocationSizes::new(8 * MB, 4 * MB)
            .with_max_device_memblock_size(20 * MB)
            .with_max_host_memblock_size(12 * MB);

        assert_eq!(
            sizes.block_sizes_for(false).shared_block_size(0, 18 * MB),
            20 * MB
        );
        assert_eq!(
            sizes.block_sizes_for(true).shared_block_size(0, 10 * MB),
            12 * MB
        );
    }

    #[test]
    fn fixed_sizes_remain_unchanged() {
        let sizes = AllocationSizes::new(256 * MB, 64 * MB);
        assert_eq!(
            sizes.block_sizes_for(false).shared_block_size(0, MB),
            256 * MB
        );
        assert_eq!(
            sizes.block_sizes_for(false).shared_block_size(0, 256 * MB),
            256 * MB
        );
        assert_eq!(
            sizes.block_sizes_for(true).shared_block_size(0, 64 * MB),
            64 * MB
        );
    }

    #[test]
    fn boundary_and_non_power_of_two_min_sizes() {
        let sizes = AllocationSizes::new(12 * MB, 12 * MB)
            .with_max_device_memblock_size(100 * MB)
            .with_max_host_memblock_size(40 * MB);

        assert_eq!(
            sizes.block_sizes_for(false).shared_block_size(0, 0),
            12 * MB
        );
        assert_eq!(
            sizes.block_sizes_for(false).shared_block_size(0, 20 * MB),
            24 * MB
        );
        assert_eq!(
            sizes.block_sizes_for(false).shared_block_size(0, 40 * MB),
            48 * MB
        );
        assert_eq!(
            sizes.block_sizes_for(false).shared_block_size(0, 96 * MB),
            96 * MB
        );
        assert_eq!(
            sizes.block_sizes_for(false).shared_block_size(0, 100 * MB),
            100 * MB
        );
        assert_eq!(
            sizes
                .block_sizes_for(false)
                .shared_block_size(usize::MAX, u64::MAX),
            100 * MB
        );
        assert_eq!(
            sizes.block_sizes_for(true).shared_block_size(0, u64::MAX),
            40 * MB
        );
    }

    #[test]
    fn empty_cache_is_bounded_and_prefers_the_larger_block() {
        let range = AllocationSizes::new(8 * MB, 4 * MB)
            .with_max_device_memblock_size(64 * MB)
            .block_sizes_for(false);
        assert!(range.should_cache_empty_block(8 * MB, None));
        assert!(range.should_cache_empty_block(16 * MB, Some(8 * MB)));
        assert!(!range.should_cache_empty_block(8 * MB, Some(16 * MB)));
        assert!(!range.should_cache_empty_block(16 * MB, Some(16 * MB)));
        assert!(!range.should_cache_empty_block(16 * MB + 1, None));
        let fixed = AllocationSizes::default().block_sizes_for(true);
        assert!(fixed.should_cache_empty_block(64 * MB, None));
        assert!(!fixed.should_cache_empty_block(64 * MB + 1, None));
    }

    #[test]
    fn heap_caps_preserve_every_fitting_request_and_bound_growth() {
        for capacity in [0, 1, 2 * MB, 4 * MB - 1, 214 * MB + 123, u64::MAX] {
            for minimum in [4 * MB, 12 * MB, 256 * MB] {
                let range = AllocationSizes::new(minimum, minimum)
                    .with_max_device_memblock_size(256 * MB)
                    .block_sizes_for(false)
                    .capped_to_heap(capacity);
                assert!(range.min <= range.max && range.max <= capacity);
                if range.max == 0 {
                    continue;
                }
                for request in [1, range.min, range.max] {
                    for count in [0, 1, 7, usize::MAX] {
                        let size = range.shared_block_size(count, request);
                        assert!(size >= request && size >= range.min && size <= range.max);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod pressure_size_tests {
    #[test]
    fn aligned_backoff_terminates_at_the_resource_floor() {
        for alignment in [1, 4 * 1024 * 1024] {
            let floor = alignment;
            for mut current in [floor, floor + 1, 3 * floor, u64::MAX] {
                let mut count = 0;
                while let Some(next) = super::smaller_block_size(current, floor, alignment) {
                    assert!(next < current && next >= floor);
                    assert_eq!(next % alignment, 0);
                    current = next;
                    count += 1;
                    assert!(count <= 64);
                }
                assert_eq!(current, floor);
            }
        }
    }
}
