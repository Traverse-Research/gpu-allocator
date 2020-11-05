use thiserror::Error;

#[derive(Error, Debug)]
pub enum AllocationError {
    #[error("Out of memory")]
    OutOfMemory,
    #[error("Failed to map memory")]
    FailedToMap,
    #[error("No compatible memory type available")]
    NoCompatibleMemoryTypeFound,
    #[error("Invalid AllocationCreateDesc")]
    InvalidAllocationCreateDesc,
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

pub type Result<V, E = AllocationError> = ::std::result::Result<V, E>;
