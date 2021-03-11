# 📒 gpu-allocator

[![Actions Status](https://github.com/Traverse-Research/gpu-allocator/workflows/CI/badge.svg)](https://github.com/Traverse-Research/gpu-allocator/actions)
[![Docs](https://docs.rs/gpu-allocator/badge.svg)](https://docs.rs/gpu-allocator/)
[![LICENSE](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE-MIT)
[![LICENSE](https://img.shields.io/badge/license-apache-blue.svg)](LICENSE-APACHE)
[![Contributor Covenant](https://img.shields.io/badge/contributor%20covenant-v1.4%20adopted-ff69b4.svg)](../main/CODE_OF_CONDUCT.md)
[![Latest version](https://img.shields.io/crates/v/gpu-allocator.svg)](https://crates.io/crates/gpu-allocator)

[![Banner](banner.png)](https://traverseresearch.nl)

```toml
[dependencies]
gpu-allocator = "0.6.0"
```

## Setting up the allocator for Vulkan
```rust
use ash::version::{DeviceV1_0, EntryV1_0, InstanceV1_0};
use ash::vk;

let mut allocator = VulkanAllocator::new(&VulkanAllocatorCreateDesc {
    instance,
    device,
    physical_device,
    debug_settings: Default::default(),
});
```

## Vulkan allocation example
```rust
// Setup vulkan info
let vk_info = vk::BufferCreateInfo::builder()
    .size(512)
    .usage(vk::BufferUsageFlags::STORAGE_BUFFER);

let buffer = unsafe { device.create_buffer(&vk_info, None) }?;
let requirements = unsafe { device.get_buffer_memory_requirements(buffer) };


let allocation = allocator
    .allocate(&AllocationCreateDesc {
        name: "Example allocation",
        requirements,
        location: MemoryLocation::CpuToGpu,
        linear: true, // Buffers are always linear
    })?;

// Bind memory to the buffer
unsafe { device.bind_buffer_memory(buffer, allocation.memory(), allocation.offset())? };

// Cleanup
allocator.free(allocation)?;
unsafe { device.destroy_buffer(buffer, None) };
```

### License

Licensed under either of

* Apache License, Version 2.0, ([LICENSE-APACHE](../master/LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
* MIT license ([LICENSE-MIT](../master/LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

### Alternative libraries
* [vk-mem-rs](https://github.com/gwihlidal/vk-mem-rs)

### Contribution

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
