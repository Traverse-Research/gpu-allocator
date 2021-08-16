use crate::result::*;

pub(crate) mod dedicated_block_allocator;
pub(crate) use dedicated_block_allocator::DedicatedBlockAllocator;

pub(crate) mod free_list_allocator;
pub(crate) use free_list_allocator::FreeListAllocator;

use log::*;

#[derive(PartialEq, Copy, Clone, Debug)]
#[repr(u8)]
pub(crate) enum AllocationType {
    Free,
    Linear,
    NonLinear,
}

#[cfg(feature = "visualizer")]
pub(crate) trait SubAllocatorBase: crate::visualizer::SubAllocatorVisualizer {}
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

    fn free(&mut self, chunk_id: Option<std::num::NonZeroU64>) -> Result<()>;

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
