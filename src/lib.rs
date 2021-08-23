//! This crate provides a fully written in Rust memory allocator for Vulkan and DirectX 12.
//!
//! ## Setting up the Vulkan memory allocator
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
//! });
//! # }
//! # #[cfg(not(feature = "vulkan"))]
//! # fn main() {}
//! ```
//!
//! ## Simple Vulkan allocation example
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
//!
//! # let mut allocator = Allocator::new(&AllocatorCreateDesc {
//! #     instance,
//! #     device,
//! #     physical_device,
//! #     debug_settings: Default::default(),
//! #     buffer_device_address: true,  // Ideally, check the BufferDeviceAddressFeatures struct.
//! # }).unwrap();
//!
//! // Setup vulkan info
//! let vk_info = vk::BufferCreateInfo::builder()
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
//! ## Setting up the D3D12 memory allocator
//!
//! ```no_run
//! # #[cfg(feature = "d3d12")]
//! # fn main() {
//! use gpu_allocator::d3d12::*;
//! # use winapi::um::d3d12;
//! # let device = todo!();
//!
//! let mut allocator = Allocator::new(&AllocatorCreateDesc {
//!     device,
//!     debug_settings: Default::default(),
//! });
//! # }
//! # #[cfg(not(feature = "d3d12"))]
//! # fn main() {}
//! ```
//!
//! ## Simple d3d12 allocation example
//!
//! ```no_run
//! # #[cfg(feature = "d3d12")]
//! # fn main() {
//! use gpu_allocator::d3d12::*;
//! use gpu_allocator::MemoryLocation;
//! # use winapi::um::d3d12;
//! # use winapi::shared::{dxgiformat, dxgitype, winerror};
//! # let device = todo!();
//!
//! # let mut allocator = Allocator::new(&AllocatorCreateDesc {
//! #     Dx12DevicePtr(device as *const _),
//! #     debug_settings: Default::default(),
//! # }).unwrap();
//!
//! let buffer_desc = d3d12::D3D12_RESOURCE_DESC {
//!     Dimension: d3d12::D3D12_RESOURCE_DIMENSION_BUFFER,
//!     Alignment: 0,
//!     Width: 512,
//!     Height: 1,
//!     DepthOrArraySize: 1,
//!     MipLevels: 1,
//!     Format: dxgiformat::DXGI_FORMAT_UNKNOWN,
//!     SampleDesc: dxgitype::DXGI_SAMPLE_DESC {
//!         Count: 1,
//!         Quality: 0,
//!     },
//!     Layout: d3d12::D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
//!     Flags: d3d12::D3D12_RESOURCE_FLAG_NONE,
//! };
//! let allocation_desc = AllocationCreateDesc::from_d3d12_resource_desc(
//!     allocator.device(),
//!     &buffer_desc,
//!     "Example allocation",
//!     MemoryLocation::GpuOnly,
//! );
//! let allocation = allocator.allocate(&allocation_desc).unwrap();
//! let mut resource: *mut d3d12::ID3D12Resource = std::ptr::null_mut();
//! let hr = unsafe {
//!     device.as_ref().unwrap().CreatePlacedResource(
//!         allocation.heap().as_winapi(),
//!         allocation.offset(),
//!         &buffer_desc,
//!         d3d12::D3D12_RESOURCE_STATE_COMMON,
//!         std::ptr::null(),
//!         &d3d12::IID_ID3D12Resource,
//!         &mut resource as *mut _ as *mut _,
//!     )
//! };
//! if hr != winerror::S_OK {
//!     panic!("Failed to create placed resource.");
//! }
//!
//! // Cleanup
//! unsafe { resource.as_ref().unwrap().Release() };
//! allocator.free(allocation).unwrap();
//! # }
//! # #[cfg(not(feature = "d3d12"))]
//! # fn main() {}
//! ```

mod result;
pub use result::*;

pub(crate) mod allocator;

#[cfg(feature = "visualizer")]
pub mod visualizer;

#[cfg(all(not(any(target_os = "macos", target_os = "ios")), feature = "vulkan"))]
pub mod vulkan;

#[cfg(all(windows, feature = "d3d12"))]
pub mod d3d12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

#[derive(Copy, Clone, Debug)]
pub struct AllocatorDebugSettings {
    /// Logs out debugging information about the various heaps the current device has on startup
    pub log_memory_information: bool,
    /// Logs out all memory leaks on shutdown with log level Warn
    pub log_leaks_on_shutdown: bool,
    /// Stores a copy of the full backtrace for every allocation made, this makes it easier to debug leaks
    /// or other memory allocations, but storing stack traces has a RAM overhead so should be disabled
    /// in shipping applications.
    pub store_stack_traces: bool,
    /// Log out every allocation as it's being made with log level Debug, rather spammy so off by default
    pub log_allocations: bool,
    /// Log out every free that is being called with log level Debug, rather spammy so off by default
    pub log_frees: bool,
    /// Log out stack traces when either `log_allocations` or `log_frees` is enabled.
    pub log_stack_traces: bool,
}

impl Default for AllocatorDebugSettings {
    fn default() -> Self {
        Self {
            log_memory_information: false,
            log_leaks_on_shutdown: true,
            store_stack_traces: false,
            log_allocations: false,
            log_frees: false,
            log_stack_traces: false,
        }
    }
}
