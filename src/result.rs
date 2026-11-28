use alloc::string::String;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum AllocationError {
    #[error("Out of memory")]
    OutOfMemory,
    #[error("Failed to map memory: {0}")]
    FailedToMap(String),
    #[error("No compatible memory type available")]
    NoCompatibleMemoryTypeFound,
    #[error("Invalid AllocationCreateDesc")]
    InvalidAllocationCreateDesc,
    #[error("Invalid AllocatorCreateDesc {0}")]
    InvalidAllocatorCreateDesc(String),
    #[error("Internal error: {0}")]
    Internal(String),
    #[error("Initial `BARRIER_LAYOUT` needs at least `Device10`")]
    BarrierLayoutNeedsDevice10,
    #[error("Castable formats require enhanced barriers")]
    CastableFormatsRequiresEnhancedBarriers,
    #[error("Castable formats require at least `Device12`")]
    CastableFormatsRequiresAtLeastDevice12,
}

pub type Result<V, E = AllocationError> = ::core::result::Result<V, E>;

#[derive(Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Allocation-count limits are reported by Vulkan only.
enum FailureKind {
    Memory,
    AllocationCount,
    Other,
}

pub(crate) struct AllocationFailure {
    pub(crate) error: AllocationError,
    kind: FailureKind,
}

#[allow(dead_code)]
impl AllocationFailure {
    pub(crate) fn memory(error: AllocationError) -> Self {
        Self {
            error,
            kind: FailureKind::Memory,
        }
    }
    pub(crate) fn fatal(error: AllocationError) -> Self {
        let error = match error {
            AllocationError::OutOfMemory => {
                AllocationError::Internal("Allocation from a new memory block failed".into())
            }
            error => error,
        };
        Self {
            error,
            kind: FailureKind::Other,
        }
    }
    pub(crate) fn allocation_limit(error: AllocationError) -> Self {
        Self {
            error,
            kind: FailureKind::AllocationCount,
        }
    }
    pub(crate) fn is_memory_pressure(&self) -> bool {
        self.kind != FailureKind::Other
    }
    pub(crate) fn may_shrink(&self) -> bool {
        self.kind == FailureKind::Memory
    }
    pub(crate) fn is_allocation_limit(&self) -> bool {
        self.kind == FailureKind::AllocationCount
    }
}

impl From<AllocationError> for AllocationFailure {
    fn from(error: AllocationError) -> Self {
        let kind = if matches!(
            &error,
            AllocationError::OutOfMemory | AllocationError::FailedToMap(_)
        ) {
            FailureKind::Memory
        } else {
            FailureKind::Other
        };
        Self { error, kind }
    }
}

pub(crate) type AllocationResult<T> = core::result::Result<T, AllocationFailure>;
