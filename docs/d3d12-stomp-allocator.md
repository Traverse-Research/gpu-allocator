# D3D12 stomp allocator: implementation plan

Debug-only mode for the D3D12 backend that places every guarded buffer so that a GPU
write past its end (or, optionally, before its start) hits a page that has never been
resident. The GPU page-faults and the device is removed with `DXGI_ERROR_DEVICE_HUNG`.
Freed resources can be quarantined so that use-after-free faults too.

This document is written so that it can be executed top to bottom without further
context. Sections 1, 3, 4 and 6 describe the design as implemented in
`src/d3d12/stomp.rs` after the spike; section 8 records the design that was tried
first and why it was rejected.

## 1. Mechanism and why it is shaped this way

Facts that constrain the design. Do not re-derive them.

1. Residency in D3D12 is per heap. A heap created with
   `D3D12_HEAP_FLAG_CREATE_NOT_RESIDENT` and never passed to `MakeResident` has no
   physical backing. Any GPU access to it page-faults. `Evict` is only a hint and must
   not be used for this purpose (Jesse Natalie, Microsoft).
2. Unmapped (NULL) tiles in reserved resources do not fault on tiled tier 2 hardware:
   reads return zero, writes are dropped. They are useless as guards.
3. D3D12 gives no control over where a heap lands in GPU virtual address (VA) space.
   Only reserved resources give VA control. Section 8 has the measurements that show
   heap adjacency cannot be relied on.
4. Textures have no GPU VA and all view accesses are hardware bounds-checked. The
   unbounded write paths in D3D12 are root descriptors (raw VA), acceleration structure
   builds and scratch, DXR shader tables and ExecuteIndirect arguments. Copy commands
   are rejected by the runtime at `Close()` when they overrun, so they never reach a
   guard. Guarding buffers is what catches real stomps.
5. `Allocator::allocate()` returns a heap and an offset for the caller's own
   `CreatePlacedResource`. That path cannot be guarded and stays untouched.
   Only `Allocator::create_resource()` is affected.
6. `UpdateTileMappings` is a command queue operation. Stomp mode therefore needs a
   queue and a fence; the allocator CPU-waits after each mapping update so the
   resource is usable on any queue when `create_resource` returns.

Resulting layout for one stomp buffer, all inside one reserved resource:

```
tile 0            : head guard  (optional; mapped to the never-resident heap)
tiles h .. h+n    : payload     (mapped to a 64KB-aligned range of a normal allocator block)
tile h+n          : tail guard  (default on; mapped to the never-resident heap)
```

`h` is 1 with a head guard, else 0; `n = ceil(size / 64KB)`. The never-resident heap is
one 64KB heap per memory type, created lazily, mapped with
`D3D12_TILE_RANGE_FLAG_REUSE_SINGLE_TILE` so every guard tile of every resource shares
it. Adjacency is by construction, so this holds under churn and across threads.

A reserved resource always starts on a tile boundary, so the tail guard is 64KB precise
by default. With `StompSettings::payload_alignment` set, the payload is packed against
the tail guard at `Resource::offset()` (rounded down to that alignment) and the caller
adds the offset to every VA and view. Ignoring the offset only widens the slack.

Textures become reserved resources with `D3D12_TEXTURE_LAYOUT_64KB_UNDEFINED_SWIZZLE`
and exact dimensions, all tiles mapped to allocator memory, no guard tiles. They gain
use-after-free detection only.

Quarantine: on `free_resource`, all tiles of a stomp resource are remapped to the
never-resident heap and the `ID3D12Resource` is kept alive until the allocator drops.
A stale descriptor or stale VA then faults. VA is retained, memory is returned to the
sub-allocator immediately.

## 2. Spike (done, `examples/d3d12-stomp.rs`)

Measured on an NVIDIA RTX 5070 Ti, resource heap tier 2, tiled resources tier 3:

- Access to a `CREATE_NOT_RESIDENT` heap removes the device with
  `DXGI_ERROR_DEVICE_HUNG`. Root UAV write, root SRV read and texture guard access all
  fault. In-bounds and slack accesses complete.
- DRED does not attribute the fault on this driver: `PageFaultVA 0`, no allocation
  nodes, even for a bogus VA. Breadcrumbs work. Treat naming as informational.
- `CopyBufferRegion` cannot overrun: the runtime rejects it at `Close()` with
  `E_INVALIDARG`.
- After the second device removal in one process, `D3D12CreateDevice` keeps failing
  with `DEVICE_HUNG`. Fault tests run each body in a child process.
- Heap VA adjacency (the first design, section 8) fails under churn.

## 3. Public API (`src/d3d12/stomp.rs`, re-exported from `gpu_allocator::d3d12`)

```rust
#[derive(Clone, Debug)]
pub struct StompSettings {
    /// Queue that executes the tile mapping updates. Use a dedicated queue (COPY is fine).
    pub queue: ID3D12CommandQueue,
    pub mode: StompMode,
    /// Seed for `StompMode::Random`. Same seed, same sequence of decisions.
    pub seed: u64,
    /// Guard tile after the payload. Default true: most stomps run forward.
    pub tail_guard: bool,
    /// Guard tile before the payload. Default false.
    pub head_guard: bool,
    /// Pack the payload against the tail guard, rounded down to this power of two (<= 65536).
    /// None keeps offset() == 0.
    pub payload_alignment: Option<u64>,
    /// Keep freed resources alive with all tiles mapped to the never-resident heap.
    pub quarantine: bool,
}
impl StompSettings { pub fn new(queue: ID3D12CommandQueue) -> Self }  // All, tail on, head off, None, quarantine on

#[derive(Clone, Copy, Debug)]
pub enum StompMode { All, Random { probability: f32 }, OptIn, Filter(fn(&ResourceCreateDesc<'_>) -> bool) }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StompLayout {
    pub resource_va: u64,      // GetGPUVirtualAddress() of the reserved resource, 0 for textures
    pub offset: u64,           // payload byte offset inside the resource
    pub payload_tiles: u32,
    pub head_guard: bool,
    pub tail_guard: bool,
}
impl StompLayout {
    pub fn head_guard_va(&self) -> Option<u64>;   // resource_va
    pub fn tail_guard_va(&self) -> Option<u64>;   // resource_va + (head + payload_tiles) * 64KB
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StompStatistics { pub guarded: usize, pub unguarded_fallbacks: usize, pub quarantined: usize }
```

- `AllocatorCreateDesc::stomp: Option<StompSettings>`; `None` keeps today's behaviour.
- `ResourceCreateDesc::stomp: Option<bool>`; `Some` overrides the mode.
- `Resource::offset()`, `Resource::is_stomp_guarded()` (buffers with a guard),
  `Resource::stomp_layout()`.
- `Allocator::stomp_statistics()`, `Allocator::committed_statistics()`.
- `AllocationError::InvalidStompSettings(String)` from `Allocator::new` for
  `probability` outside `0.0..=1.0`, both guards off, `payload_alignment` not a power of
  two or above 65536, and tiled resources tier 0.
- `guarded` counts every resource that took the stomp path, textures included;
  `unguarded_fallbacks` counts refusals (multisampled, 3D below tiled tier 3, and
  `CpuToGpu` / `GpuToCpu` because reserved resources cannot be `Map()`ed, verified:
  `E_INVALIDARG`), which go to the normal path with one `warn!`.

## 4. Internals

`src/d3d12/mod.rs` keeps only the hooks: the `stomp` fields on the descs, `Resource`
(`stomp: Option<StompLayout>`, `offset`), `Allocator` (`stomp: Option<StompState>`) and
`MemoryType` (`guard_heap: Option<ID3D12Heap>`); `StompState::new` in `Allocator::new`;
`try_create_resource_stomp` at the top of `create_resource`; `release_resource` in
`free_resource`; and the drop order in `Drop for Allocator` (quarantine, then memory
blocks, then guard heaps). Shared helpers stay in `mod.rs`: `create_heap`, `heap_flags`,
`find_memory_type_index`, `create_placed`.

`src/d3d12/stomp.rs` holds everything else:

- Pure functions, unit-tested without a device: `xorshift64`, `stomp_decide`,
  `stomp_geometry(size, tail_guard, head_guard, payload_alignment) -> (payload_tiles,
  offset)`, `validate_stomp_settings`.
- `StompState { settings, rng, tiled_tier, fence, fence_value, guarded,
  unguarded_fallbacks, quarantine: Vec<ID3D12Resource> }`.
- `MemoryType::guard_heap()`: lazy 64KB heap with the category flags plus
  `CREATE_NOT_RESIDENT`, never made resident.
- `Allocator::create_resource_stomp`:
  1. Buffers: `Width = (h + n + t) * 64KB`, `Alignment = 0`. Textures: `Layout =
     64KB_UNDEFINED_SWIZZLE`, `Alignment = 0`, no guards.
  2. `CreateReservedResource` (or `CreateReservedResource2` on Device10/Device12 for
     barrier layouts and castable formats), same error mapping as `create_placed`.
  3. Textures: `GetResourceTiling` for `NumTilesForEntireResource`.
  4. Payload memory through the existing `allocate()` with `size = payload_tiles * 64KB`,
     `alignment = 64KB`, so leak reports, visualizer and `free` are unchanged.
  5. `UpdateTileMappings` on `settings.queue`: payload region to the allocation's heap at
     `offset / 64KB`; head and tail tiles to the guard heap with `REUSE_SINGLE_TILE`.
  6. `queue.Signal(fence)` then `fence.SetEventOnCompletion(value, HANDLE::default())`,
     which blocks on a null handle. On error, free the allocation and return the error.
  7. `SetName(desc.name)`, return `Resource { allocation: Some, stomp: Some(layout), offset }`.
- `Allocator::release_resource` (free side): with quarantine on, remap all
  `h + n + t` tiles to the guard heap, wait, push the resource into `quarantine`;
  otherwise drop it. Then the normal `free(allocation)`.

## 5. Example: `examples/d3d12-stomp.rs`

DRED on, allocator with `StompSettings::new(copy_queue)`, 4000-byte buffer named
`"stomp me"`, root UAV compute write at `tail_guard_va()`, execute, fence wait, expect
device removal, print the reason and whatever DRED attributes. Exit 0 when the guard
access removed the device. Falls back to WARP with a note that WARP does not fault.

## 6. Tests

Three layers. Layers 1 and 2 run on CI (`windows-latest` has WARP). Layer 3 needs real
hardware because WARP does not page-fault.

### 6.1 Layer 1: unit tests, no device (`src/d3d12/stomp.rs`, `mod tests`)

Geometry table, `(size, tail, head, payload_alignment) -> (payload_tiles, offset)`:

| size  | tail  | head  | alignment | tiles | offset        |
|-------|-------|-------|-----------|-------|---------------|
| 4000  | true  | false | Some(8)   | 1     | 61536         |
| 4000  | true  | false | None      | 1     | 0             |
| 4000  | true  | true  | Some(256) | 1     | 65536 + 61440 |
| 4000  | false | true  | Some(8)   | 1     | 65536         |
| 65536 | true  | false | Some(8)   | 1     | 0             |
| 65537 | true  | false | Some(8)   | 2     | 65528         |
| 70000 | true  | false | Some(256) | 2     | 60928         |
| 1     | true  | false | Some(65536) | 1   | 0             |
| 1     | true  | false | Some(8)   | 1     | 65528         |

Property check over 10k random sizes and alignments, with and without head guard:
`offset % alignment == 0`, `offset >= head * 64KB`, `offset + size <= (head + tiles) *
64KB`, slack under `alignment` when `Some`, and `offset == head * 64KB` when `None`.

Random mode bounds and determinism, decision matrix for all modes and overrides,
settings validation (probability, both guards off, alignment 0 / 3 / 131072),
`heap_flags` mapping.

### 6.2 Layer 2: device-backed integration tests (`tests/d3d12_stomp.rs`)

Gated `#![cfg(all(windows, feature = "d3d12"))]`. Helpers: `make_device()` with the
debug layer, hardware else WARP, panic if neither; `assert_no_d3d12_errors()` sweeping
`ID3D12InfoQueue` for `ERROR`/`CORRUPTION` at the end of every test; a copy queue for
`StompSettings::new`. Each test creates its own device and allocator.

1. `plain_allocator_unchanged`: no stomp settings; `stomp_statistics()` is `None`,
   `stomp_layout()` is `None`, `offset() == 0`.
2. `all_mode_stomps_buffer`: 4000-byte `GpuOnly` buffer; `allocation.is_some()`,
   `is_stomp_guarded()`, layout has `tail_guard`, no `head_guard`, `payload_tiles == 1`,
   `offset == 0`; `guarded == 1`, `unguarded_fallbacks == 0`.
3. `width_covers_tiles`: `GetDesc().Width == (payload_tiles + 1) * 65536`.
4. `tail_guard_va_follows_payload`: `tail_guard_va() == resource_va + 65536`;
   `GetGPUVirtualAddress() == resource_va`.
5. `payload_alignment_packs_against_tail`: `payload_alignment: Some(256)`; `offset ==
   61440`, `offset + 4000 <= 65536`, slack under 256.
6. `head_guard_layout`: `head_guard: true, tail_guard: true`; `head_guard_va() ==
   resource_va`, payload starts at tile 1 (`offset == 65536` with `None` alignment),
   `tail_guard_va() == resource_va + 2 * 65536`.
7. `head_only`: `head_guard: true, tail_guard: false`; `offset == 65536`,
   `tail_guard_va()` is `None`.
8. `random_zero_guards_none` / `random_one_guards_all`: 50 buffers each.
9. `random_is_deterministic`: two allocators, same seed, identical decision vectors.
10. `opt_in_mode` and `Some(false)` under `All`.
11. `filter_mode`: buffers stomped, textures on the normal path.
12. `texture_takes_stomp_path`: 256x256 texture; `stomp_layout().is_some()`,
    `is_stomp_guarded() == false`, `resource_va == 0`, `payload_tiles ==
    GetResourceTiling` count, `GetDesc()` dimensions unchanged, `Layout ==
    64KB_UNDEFINED_SWIZZLE`.
13. `rt_texture_takes_stomp_path`: same with `ALLOW_RENDER_TARGET` and a clear value.
14. `msaa_falls_back`: 4x MSAA render target on the normal path, `unguarded_fallbacks == 1`.
15. `cpu_heaps_fall_back_and_stay_mappable`: `CpuToGpu` and `GpuToCpu` buffers under
    `All` take the normal path (`unguarded_fallbacks` incremented) and `Map()` works.
    Reserved resources reject `Map()` with `E_INVALIDARG`, measured.
16. `committed_request_takes_stomp_path`.
17. `free_quarantines`: create 20, free all; `quarantined == 20`, `generate_report()`
    shows zero live allocations, no D3D12 errors.
18. `free_without_quarantine`: `quarantine: false`; `quarantined == 0` after frees.
19. `churn_no_errors`: 500 random-size buffers (1 byte to 4MB), ring of 32; `guarded ==
    500`, `unguarded_fallbacks == 0`, every resource `is_stomp_guarded()`, no D3D12
    errors. This is the invariant plan B could not meet.
20. `drop_order_is_safe`: drop the allocator with quarantined resources, with a live
    stomp resource (expect the "not freed" warning), and after freeing everything.
21. `rename_and_report_unaffected`.
22. `tiled_tier_required`: skip unless the adapter reports tier 0 (WARP is tier 3).
23. `threads_do_not_break_guards`: 4 threads each with its own allocator on the same
    device, 100 buffers each; all guarded.

### 6.3 Layer 3: fault tests, hardware only (`tests/d3d12_stomp_fault.rs`)

All `#[ignore]`, early return unless `GPU_ALLOCATOR_STOMP_FAULT_TESTS=1`, each body in a
child process (device creation fails after two removals in one process), run with
`--test-threads=1`. Root-descriptor compute shaders under `tests/shaders/` with the dxc
command in each `.hlsl` header.

1. `tail_overrun_faults`: root UAV write at `tail_guard_va()`; expect removal.
2. `in_bounds_write_does_not_fault`: write at `resource_va + offset + size - 4`.
3. `slack_write_does_not_fault`: with `payload_alignment: Some(8)`, write at
   `tail_guard_va() - 4` (in the slack or the payload), no fault; write at
   `tail_guard_va()`, fault.
4. `head_underrun_faults`: `head_guard: true`; root SRV read at `head_guard_va()`.
5. `use_after_free_faults`: quarantine on; keep a root UAV to `resource_va` of a buffer,
   free it, write; expect removal. With `quarantine: false` the same write must not
   fault (memory may be reused, but no page is missing).
6. `texture_use_after_free_faults`: keep a descriptor to a stomp texture, free it,
   sample it in a compute shader; expect removal.
7. `dred_attribution`: informational; print `PageFaultVA` and allocation node names.

### 6.4 CI wiring

- `tests/d3d12_stomp.rs` runs on the existing `windows-latest` job via
  `cargo test --workspace --all-targets --features d3d12,...` on WARP.
- Layer 3:
  `GPU_ALLOCATOR_STOMP_FAULT_TESTS=1 cargo test --features d3d12 --test d3d12_stomp_fault -- --ignored --test-threads=1`.

## 7. Documentation

The `StompSettings` doc comment covers what is caught and what is not, the offset
contract, `GetDesc().Width` and `CopyResource`, quarantine, the queue requirement, DRED
enablement and its limits on NVIDIA, and WARP. `src/lib.rs` points at `StompSettings`
under the D3D12 setup section; regenerate `README.md` with `cargo readme > README.md`.

## 8. Rejected first design: adjacent heaps with tight placed resource alignment

The first implementation gave every stomp resource its own heap, placed the resource at
the end of that heap (byte-exact with `D3D12_RESOURCE_FLAG_USE_TIGHT_ALIGNMENT`, Agility
SDK 1.618.1 and newer), created a 64KB `CREATE_NOT_RESIDENT` heap right after it, and
verified adjacency by reading the GPU VA of throwaway placed buffers, retrying on a miss.
No offset contract, no queue, exact `Width`. Measurements on NVIDIA:

- Raw heap pairs (payload then guard, dropped each iteration): 0.0 percent adjacent in
  steady state. Freed VA is reused by size class; a 64KB guard lands in any 64KB hole.
- Guard heap sized equal to the payload: 96.9 percent for 64KB payloads, 7.8 percent
  for random 64KB to 16MB payloads.
- Through the allocator, 500 buffers of 1 byte to 4MB with a ring of 32 live: 55 of 500
  guarded with 0 retries, 72 with 4, 105 with 16. Payloads that are 2MB multiples never
  got an adjacent guard even on fresh VA.
- The driver's VA allocator is process-wide across devices and threads, so heaps
  created on another thread land between payload and guard.

A guard that holds for the first allocation and then silently degrades is worse than
none. Tight alignment is unusable with reserved resources (spec: `E_INVALIDARG`), so it
plays no part in the current design.

## 9. Verification checklist

```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --no-default-features --features d3d12,std -- -D warnings
cargo test --workspace --all-targets --no-default-features --features d3d12,std
cargo doc --no-deps --workspace --all-features --document-private-items
cargo readme > README.md && git diff --quiet README.md
cargo run --example d3d12-stomp --features d3d12     # on a real GPU, expect exit code 0
GPU_ALLOCATOR_STOMP_FAULT_TESTS=1 cargo test --features d3d12 --test d3d12_stomp_fault -- --ignored --test-threads=1   # real GPU only
```

Also run the existing `d3d12-buffer-winrs` example to confirm the non-stomp path is
untouched.
