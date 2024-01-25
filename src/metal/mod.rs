#![deny(clippy::unimplemented, clippy::unwrap_used, clippy::ok_expect)]
use crate::Result;
use log::Level;

pub struct Allocation {}
pub struct AllocationCreateDesc {}
pub struct Allocator {}
pub struct AllocatorCreateDesc {}
pub struct ResourceCreateDesc {}
pub struct Resource {}

impl Allocator {
    pub fn device(&self) -> &metal::Device {
        todo!()
    }
    pub fn new(_desc: &AllocatorCreateDesc) -> Result<Self> {
        todo!()
    }
    pub fn allocate(&mut self, _desc: &AllocationCreateDesc) -> Result<Allocation> {
        todo!()
    }
    pub fn free(&mut self, _allocation: Allocation) -> Result<()> {
        todo!()
    }
    pub fn rename_allocation(&mut self, _allocation: &mut Allocation, _name: &str) -> Result<()> {
        todo!()
    }
    pub fn report_memory_leaks(&self, _log_level: Level) {
        todo!()
    }
    pub fn create_resource(&mut self, _desc: &ResourceCreateDesc) -> Result<Resource> {
        todo!()
    }
    pub fn free_resource(&mut self, mut _resource: Resource) -> Result<()> {
        todo!()
    }
}
