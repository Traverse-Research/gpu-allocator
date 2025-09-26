# 📒 gpu-allocator

[![Actions Status](https://img.shields.io/github/actions/workflow/status/Traverse-Research/gpu-allocator/ci.yml?branch=main&logo=github)](https://github.com/Traverse-Research/gpu-allocator/actions)
[![Latest version](https://img.shields.io/crates/v/gpu-allocator.svg?logo=rust)](https://crates.io/crates/gpu-allocator)
[![Docs](https://img.shields.io/docsrs/gpu-allocator?logo=docs.rs)](https://docs.rs/gpu-allocator/)
[![LICENSE](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE-MIT)
[![LICENSE](https://img.shields.io/badge/license-apache-blue.svg?logo=apache)](LICENSE-APACHE)
[![Contributor Covenant](https://img.shields.io/badge/contributor%20covenant-v1.4%20adopted-ff69b4.svg)](../main/CODE_OF_CONDUCT.md)
[![MSRV](https://img.shields.io/badge/rustc-1.71.0+-ab6000.svg)](https://blog.rust-lang.org/2023/07/13/Rust-1.71.0.html)

[![Banner](banner.png)](https://traverseresearch.nl)

```toml
[dependencies]
gpu-allocator = "0.28.0"
```

![Visualizer](visualizer.png)

{{readme}}

## `no_std` support

`no_std` support can be enabled by compiling with `--no-default-features` to disable `std` support and `--features hashbrown` for `Hash` collections that are only defined in `std` for internal usages in crate. For example:

```toml
[dependencies]
gpu-allocator = { version = "0.28.0", default-features = false, features = ["hashbrown", "other features"] }
```

To support both `std` and `no_std` builds in your project, use the following in your `Cargo.toml`:

```toml
[features]
default = ["std", "other features"]

std = ["gpu-allocator/std"]
hashbrown = ["gpu-allocator/hashbrown"]
other_features = []

[dependencies]
gpu-allocator = { version = "0.28.0", default-features = false }
```

## Minimum Supported Rust Version

The MSRV for this crate and the `vulkan`, `d3d12` and `metal` features is Rust **1.71**.

The `no_std` support requires Rust **1.81** or higher because `no_std` support of dependency `thiserror` requires `core::error::Error` which is stabilized in **1.81**.

Any other features such as the `visualizer` (with all the `egui` dependencies) may have a higher requirement and are not tested in our CI.

## License

Licensed under either of

- Apache License, Version 2.0, ([LICENSE-APACHE](../master/LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](../master/LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

## Alternative libraries

- [vk-mem-rs](https://github.com/gwihlidal/vk-mem-rs)

## Contribution

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
