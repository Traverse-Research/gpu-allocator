// #[cfg(windows)]
// pub mod dx12;

#[cfg(not(any(target_os = "macos", target_os = "ios")))]
pub mod vulkan;

mod result;
pub use result::*;

mod dedicated_block_allocator;
use dedicated_block_allocator::DedicatedBlockAllocator;

mod free_list_allocator;
use free_list_allocator::FreeListAllocator;

#[cfg(feature = "visualizer")]
pub mod visualizer;

use log::*;

pub(crate) trait SubAllocation {
    fn chunk_id(&self) -> Option<std::num::NonZeroU64>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryLocation {
    /// The allocated resource is stored at an unknown memory location; let the driver decide what's the best location
    Unknown,
    /// Store the allocation in GPU only accesible memory - typically this is the faster GPU resource and this should be
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

#[derive(PartialEq, Copy, Clone, Debug)]
#[repr(u8)]
pub enum AllocationType {
    Free,
    Linear,
    NonLinear,
}

#[cfg(feature = "visualizer")]
pub(crate) trait SubAllocatorBase: visualizer::SubAllocatorVisualizer {}
#[cfg(not(feature = "visualizer"))]
pub(crate) trait SubAllocatorBase {}

pub(crate) trait SubAllocator: SubAllocatorBase + std::fmt::Debug {
    fn allocate(
        &mut self,
        size: u64,
        alignment: u64,
        allocation_type: AllocationType,
        granularity: u64,
        name: &str,
        backtrace: Option<&str>,
    ) -> Result<(u64, std::num::NonZeroU64)>;

    fn free(&mut self, sub_allocation: Box<dyn SubAllocation>) -> Result<()>;

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
