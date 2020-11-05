use ash::vk;


pub mod memory_location;
pub use memory_location::MemoryLocation;

mod result;
pub use result::*;

mod math;

#[derive(Clone, Debug)]
pub struct AllocationCreateDesc<'a> {
    pub requirements: vk::MemoryRequirements,
    pub location: MemoryLocation,
    pub is_linear_resource: bool,
    pub name: &'a str,
}

//Gpu Allocator
pub mod gpu_allocator;
pub use gpu_allocator::{Allocator, SubAllocation};
