//! Debug-only stomp detection for the D3D12 backend, see [`StompSettings`].
//!
//! Everything stomp-specific lives here. `mod.rs` only carries the hooks: the `stomp` fields
//! on the descs, `Resource`, `Allocator` and `MemoryType`, and the calls into
//! [`Allocator::try_create_resource_stomp()`] and [`Allocator::release_resource()`].

use alloc::vec::Vec;

use log::warn;
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::HANDLE,
        Graphics::{Direct3D12::*, Dxgi::DXGI_ERROR_DEVICE_REMOVED},
    },
};

use super::{
    create_heap, heap_flags, AllocationCreateDesc, Allocator, ID3D12DeviceVersion, MemoryType,
    Resource, ResourceCreateDesc, ResourceStateOrBarrierLayout,
};
use crate::{AllocationError, MemoryLocation, Result};

/// Tile size of reserved resources, also the heap granularity used by stomp mode.
const TILE: u64 = D3D12_TILED_RESOURCE_TILE_SIZE_IN_BYTES as u64;

/// Debug-only stomp detection for resources created with [`Allocator::create_resource()`].
///
/// A stomp resource is a reserved (tiled) resource. Its payload tiles are mapped to memory
/// from the normal sub-allocator; the guard tiles before and/or after the payload are mapped
/// to a 64KB heap that was created with `D3D12_HEAP_FLAG_CREATE_NOT_RESIDENT` and is never
/// made resident. A GPU access to a guard tile page-faults and removes the device with
/// `DXGI_ERROR_DEVICE_HUNG`. Adjacency of payload and guard is guaranteed by construction, so
/// this works under allocation churn and from multiple threads.
///
/// # What is caught
///
/// Accesses that the hardware does not bounds-check: root descriptor SRV/UAV/CBV reads and
/// writes, acceleration structure builds and their scratch buffers, DXR shader tables and
/// `ExecuteIndirect` arguments. Descriptor-table views are clamped by the hardware and never
/// reach the guard. Copy commands are rejected by the runtime when they overrun, they are not
/// caught here either.
///
/// Buffers get a tail guard (default) and optionally a head guard. Textures get no guard:
/// D3D12 exposes no unbounded write path into textures. Both get use-after-free detection
/// through `quarantine`.
///
/// Only [`MemoryLocation::GpuOnly`] resources are stomped. Reserved resources cannot be
/// `Map()`ed, so `CpuToGpu` and `GpuToCpu` resources take the normal path (counted in
/// [`StompStatistics::unguarded_fallbacks`]), as do multisampled resources and 3D textures
/// below tiled resources tier 3.
///
/// [`Allocator::allocate()`] is not affected: the caller creates the placed resource there.
///
/// # Offset contract
///
/// A reserved resource always starts on a 64KB tile, so a tail guard is only 64KB precise by
/// default: a buffer of 4000 bytes has 61536 bytes of slack before the guard. Set
/// `payload_alignment` to get byte precision: the payload is then packed against the tail
/// guard and starts at [`Resource::offset()`], rounded down to that alignment. The caller
/// must add that offset to `GetGPUVirtualAddress()`, `BufferLocation`, `FirstElement` and
/// copy offsets. Ignoring the offset only widens the slack, nothing breaks.
///
/// The reserved buffer is `Width = tiles * 64KB`, so `GetDesc().Width` is larger than
/// requested and `CopyResource` between a stomp buffer and a normal one fails validation.
///
/// # Quarantine
///
/// With `quarantine` on, [`Allocator::free_resource()`] remaps every tile of the resource to
/// the never-resident heap and keeps the `ID3D12Resource` alive until the allocator is dropped.
/// A stale descriptor or stale GPU VA then faults instead of reading whatever reused the
/// memory. This keeps GPU virtual address space, not memory.
///
/// # Queue
///
/// Tile mappings are enqueued on `queue` and CPU-waited before the resource is returned. Pass
/// a dedicated queue (a `D3D12_COMMAND_LIST_TYPE_COPY` queue is fine) so the wait does not
/// stall your main queue.
///
/// # Diagnosing a fault
///
/// Enable DRED before creating the device:
///
/// ```ignore
/// let dred: ID3D12DeviceRemovedExtendedDataSettings = D3D12GetDebugInterface()?;
/// dred.SetPageFaultEnablement(D3D12_DRED_ENABLEMENT_FORCED_ON);
/// dred.SetAutoBreadcrumbsEnablement(D3D12_DRED_ENABLEMENT_FORCED_ON);
/// ```
///
/// Not every driver attributes the fault: NVIDIA returns `PageFaultVA 0` and no allocation
/// nodes, breadcrumbs still show the offending command. Resources are named after
/// [`ResourceCreateDesc::name`] for the drivers that do. WARP does not fault at all.
///
/// Development builds only: every stomp resource costs a reserved resource, tile mapping
/// calls and a fence wait.
#[derive(Clone, Debug)]
pub struct StompSettings {
    /// Queue that executes the tile mapping updates. Use a dedicated queue.
    pub queue: ID3D12CommandQueue,
    pub mode: StompMode,
    /// Seed for [`StompMode::Random`]. Same seed, same sequence of decisions.
    pub seed: u64,
    /// Guard tile after the payload. Default `true`: most stomps run forward.
    pub tail_guard: bool,
    /// Guard tile before the payload. Default `false`.
    pub head_guard: bool,
    /// Pack the payload against the tail guard, rounded down to this power of two (at most
    /// 65536). `None` keeps the payload at the start of its tiles with `offset() == 0`.
    pub payload_alignment: Option<u64>,
    /// Keep freed resources alive with all tiles mapped to the never-resident heap.
    pub quarantine: bool,
}

impl StompSettings {
    /// Mode `All`, tail guard only, no payload alignment, quarantine on.
    pub fn new(queue: ID3D12CommandQueue) -> Self {
        Self {
            queue,
            mode: StompMode::All,
            seed: 0x9E37_79B9_7F4A_7C15,
            tail_guard: true,
            head_guard: false,
            payload_alignment: None,
            quarantine: true,
        }
    }
}

/// Which resources get guarded. A per-resource [`ResourceCreateDesc::stomp`] of `Some`
/// overrides the mode.
#[derive(Clone, Copy, Debug)]
pub enum StompMode {
    /// Every resource.
    All,
    /// Each resource independently with this probability in `0.0..=1.0`.
    Random { probability: f32 },
    /// Only resources with [`ResourceCreateDesc::stomp`] set to `Some(true)`.
    OptIn,
    /// Caller decides per resource. A closure that captures nothing coerces to this,
    /// e.g. `StompMode::Filter(|d| d.resource_category == ResourceCategory::Buffer)`.
    Filter(fn(&ResourceCreateDesc<'_>) -> bool),
}

/// Tile layout of a stomp resource. Tiles are 64KB. With a head guard, tile 0 is the guard
/// and the payload starts at tile 1; the tail guard is the tile after the payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StompLayout {
    /// `GetGPUVirtualAddress()` of the reserved resource, `0` for textures.
    pub resource_va: u64,
    /// Byte offset of the payload inside the resource, see [`Resource::offset()`].
    pub offset: u64,
    /// Tiles backed by allocator memory.
    pub payload_tiles: u32,
    pub head_guard: bool,
    pub tail_guard: bool,
}

impl StompLayout {
    fn head_tiles(&self) -> u32 {
        u32::from(self.head_guard)
    }

    fn total_tiles(&self) -> u32 {
        self.head_tiles() + self.payload_tiles + u32::from(self.tail_guard)
    }

    /// GPU VA of the tail guard tile, `None` without one or for textures.
    pub fn tail_guard_va(&self) -> Option<u64> {
        (self.tail_guard && self.resource_va != 0)
            .then(|| self.resource_va + u64::from(self.head_tiles() + self.payload_tiles) * TILE)
    }

    /// GPU VA of the head guard tile, `None` without one or for textures.
    pub fn head_guard_va(&self) -> Option<u64> {
        (self.head_guard && self.resource_va != 0).then_some(self.resource_va)
    }
}

/// Counts since the allocator was created.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StompStatistics {
    /// Resources that took the stomp path (guarded buffers and quarantine-only textures).
    pub guarded: usize,
    /// Resources the stomp path refused (multisampled, or 3D below tiled tier 3).
    pub unguarded_fallbacks: usize,
    /// Freed resources kept alive with all tiles mapped to the never-resident heap.
    pub quarantined: usize,
}

pub(super) struct StompState {
    settings: StompSettings,
    rng: u64,
    tiled_tier: D3D12_TILED_RESOURCES_TIER,
    fence: ID3D12Fence,
    fence_value: u64,
    guarded: usize,
    unguarded_fallbacks: usize,
    pub(super) quarantine: Vec<ID3D12Resource>,
}

fn xorshift64(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

fn stomp_decide(
    mode: StompMode,
    per_resource: Option<bool>,
    rng: &mut u64,
    desc: &ResourceCreateDesc<'_>,
) -> bool {
    if let Some(forced) = per_resource {
        return forced;
    }
    match mode {
        StompMode::All => true,
        StompMode::OptIn => false,
        StompMode::Filter(f) => f(desc),
        StompMode::Random { probability } => {
            // 24 random bits, uniform in `0.0..1.0`, so `0.0` never and `1.0` always selects.
            let unit = (xorshift64(rng) >> 40) as f32 / (1u64 << 24) as f32;
            unit < probability
        }
    }
}

/// Payload tile count and payload byte offset for a stomp buffer of `size` bytes. With
/// `tail_guard` and a `payload_alignment` the payload is packed against the tail guard,
/// rounded down to that alignment; otherwise it starts on its first tile.
fn stomp_geometry(
    size: u64,
    tail_guard: bool,
    head_guard: bool,
    payload_alignment: Option<u64>,
) -> (u32, u64) {
    let payload_tiles = ((size + TILE - 1) / TILE) as u32;
    let slack = match payload_alignment {
        Some(alignment) if tail_guard => {
            (u64::from(payload_tiles) * TILE - size) & !(alignment - 1)
        }
        _ => 0,
    };
    (payload_tiles, u64::from(head_guard) * TILE + slack)
}

fn validate_stomp_settings(
    mode: StompMode,
    tail_guard: bool,
    head_guard: bool,
    payload_alignment: Option<u64>,
) -> Result<()> {
    if let StompMode::Random { probability } = mode {
        if !(0.0..=1.0).contains(&probability) {
            return Err(AllocationError::InvalidStompSettings(format!(
                "probability {probability} is not in 0.0..=1.0"
            )));
        }
    }
    if !tail_guard && !head_guard {
        return Err(AllocationError::InvalidStompSettings(
            "at least one of tail_guard or head_guard must be enabled".into(),
        ));
    }
    if let Some(alignment) = payload_alignment {
        if !alignment.is_power_of_two() || alignment > TILE {
            return Err(AllocationError::InvalidStompSettings(format!(
                "payload_alignment {alignment} must be a power of two <= {TILE}"
            )));
        }
    }
    Ok(())
}

impl StompState {
    pub(super) fn new(
        settings: &StompSettings,
        tiled_tier: D3D12_TILED_RESOURCES_TIER,
        device: &ID3D12Device,
    ) -> Result<Self> {
        validate_stomp_settings(
            settings.mode,
            settings.tail_guard,
            settings.head_guard,
            settings.payload_alignment,
        )?;
        if tiled_tier.0 < D3D12_TILED_RESOURCES_TIER_1.0 {
            return Err(AllocationError::InvalidStompSettings(
                "tiled resources unsupported".into(),
            ));
        }
        let fence = unsafe { device.CreateFence(0, D3D12_FENCE_FLAG_NONE) }.map_err(|e| {
            AllocationError::Internal(format!("ID3D12Device::CreateFence failed: {e}"))
        })?;
        Ok(Self {
            settings: settings.clone(),
            // xorshift never leaves state 0.
            rng: settings.seed | 1,
            tiled_tier,
            fence,
            fence_value: 0,
            guarded: 0,
            unguarded_fallbacks: 0,
            quarantine: Vec::new(),
        })
    }
}

impl MemoryType {
    fn guard_heap(&mut self, device: &ID3D12Device) -> Result<&ID3D12Heap> {
        if self.guard_heap.is_none() {
            self.guard_heap = Some(create_heap(
                device,
                TILE,
                &self.heap_properties,
                heap_flags(self.heap_category) | D3D12_HEAP_FLAG_CREATE_NOT_RESIDENT,
                TILE,
            )?);
        }
        Ok(self
            .guard_heap
            .as_ref()
            .expect("guard heap was just created"))
    }
}

impl Allocator {
    /// Stomp counts since creation, `None` without stomp mode.
    pub fn stomp_statistics(&self) -> Option<StompStatistics> {
        self.stomp.as_ref().map(|s| StompStatistics {
            guarded: s.guarded,
            unguarded_fallbacks: s.unguarded_fallbacks,
            quarantined: s.quarantine.len(),
        })
    }

    fn should_stomp(&mut self, desc: &ResourceCreateDesc<'_>) -> bool {
        match &mut self.stomp {
            Some(state) => stomp_decide(state.settings.mode, desc.stomp, &mut state.rng, desc),
            None => false,
        }
    }

    /// Hook for [`Allocator::create_resource()`]: `Some` when the resource was created (or
    /// failed) on the stomp path, `None` when the normal path should handle it.
    pub(super) fn try_create_resource_stomp(
        &mut self,
        desc: &ResourceCreateDesc<'_>,
    ) -> Option<Result<Resource>> {
        if !self.should_stomp(desc) {
            return None;
        }
        let state = self.stomp.as_mut().expect("stomp state exists");
        let d = desc.resource_desc;
        let tier3 = state.tiled_tier.0 >= D3D12_TILED_RESOURCES_TIER_3.0;
        let refused = if d.SampleDesc.Count > 1 {
            Some("multisampled")
        } else if d.Dimension == D3D12_RESOURCE_DIMENSION_TEXTURE3D && !tier3 {
            Some("3D below tiled resources tier 3")
        } else if matches!(
            desc.memory_location,
            MemoryLocation::CpuToGpu | MemoryLocation::GpuToCpu
        ) {
            // Reserved resources cannot be `Map()`ed (E_INVALIDARG), so a CPU-visible
            // stomp resource would be useless to the caller.
            Some("CPU-visible")
        } else {
            None
        };
        match refused {
            Some(why) => {
                warn!("Stomp: `{}` is {why}, not guarded", desc.name);
                state.unguarded_fallbacks += 1;
                None
            }
            None => Some(self.create_resource_stomp(desc)),
        }
    }

    /// Hook for [`Allocator::free_resource()`]: quarantines a stomp resource when enabled,
    /// otherwise releases it.
    pub(super) fn release_resource(&mut self, resource: &Resource, d3d12_resource: ID3D12Resource) {
        let quarantine = self.stomp.as_ref().is_some_and(|s| s.settings.quarantine);
        match (resource.stomp, &resource.allocation) {
            (Some(layout), Some(allocation)) if quarantine => {
                let memory_type_index = allocation.memory_type_index;
                if let Err(e) = self.quarantine(d3d12_resource, layout, memory_type_index) {
                    warn!(
                        "Stomp: could not quarantine `{}`, releasing instead: {e}",
                        resource.name
                    );
                }
            }
            _ => drop(d3d12_resource),
        }
    }

    fn wait_for_mappings(&mut self) -> Result<()> {
        let state = self
            .stomp
            .as_mut()
            .expect("stomp state exists when stomping");
        state.fence_value += 1;
        unsafe {
            state
                .settings
                .queue
                .Signal(&state.fence, state.fence_value)
                .map_err(|e| {
                    AllocationError::Internal(format!("ID3D12CommandQueue::Signal failed: {e}"))
                })?;
            // A null event blocks the calling thread until the fence reaches the value.
            state
                .fence
                .SetEventOnCompletion(state.fence_value, HANDLE::default())
                .map_err(|e| {
                    AllocationError::Internal(format!(
                        "ID3D12Fence::SetEventOnCompletion failed: {e}"
                    ))
                })
        }
    }

    /// Map `tiles` tiles starting at tile `first` of `resource` to `heap` at `heap_tile`.
    /// With `reuse_single_tile` every tile maps to the same heap tile (the guard).
    fn map_tiles(
        &self,
        resource: &ID3D12Resource,
        first: u32,
        tiles: u32,
        heap: &ID3D12Heap,
        heap_tile: u32,
        reuse_single_tile: bool,
    ) {
        let queue = &self
            .stomp
            .as_ref()
            .expect("stomp state exists")
            .settings
            .queue;
        let coordinate = D3D12_TILED_RESOURCE_COORDINATE {
            X: first,
            Y: 0,
            Z: 0,
            Subresource: 0,
        };
        let region = D3D12_TILE_REGION_SIZE {
            NumTiles: tiles,
            ..Default::default()
        };
        let range_flags = if reuse_single_tile {
            D3D12_TILE_RANGE_FLAG_REUSE_SINGLE_TILE
        } else {
            D3D12_TILE_RANGE_FLAG_NONE
        };
        unsafe {
            queue.UpdateTileMappings(
                resource,
                1,
                Some(&coordinate),
                Some(&region),
                heap,
                1,
                Some(&range_flags),
                Some(&heap_tile),
                Some(&tiles),
                D3D12_TILE_MAPPING_FLAG_NONE,
            )
        };
    }

    fn create_reserved(
        &self,
        resource_desc: &D3D12_RESOURCE_DESC,
        desc: &ResourceCreateDesc<'_>,
    ) -> Result<ID3D12Resource> {
        let clear_value: Option<*const D3D12_CLEAR_VALUE> =
            desc.clear_value.map(|v| -> *const _ { v });
        let mut result: Option<ID3D12Resource> = None;
        if let Err(e) = unsafe {
            match (&self.device, desc.initial_state_or_layout) {
                (_, ResourceStateOrBarrierLayout::ResourceState(_))
                    if !desc.castable_formats.is_empty() =>
                {
                    return Err(AllocationError::CastableFormatsRequiresEnhancedBarriers)
                }
                (
                    ID3D12DeviceVersion::Device12(device),
                    ResourceStateOrBarrierLayout::BarrierLayout(initial_layout),
                ) => device.CreateReservedResource2(
                    resource_desc,
                    initial_layout,
                    clear_value,
                    None,
                    Some(desc.castable_formats),
                    &mut result,
                ),
                (_, ResourceStateOrBarrierLayout::BarrierLayout(_))
                    if !desc.castable_formats.is_empty() =>
                {
                    return Err(AllocationError::CastableFormatsRequiresAtLeastDevice12)
                }
                (
                    ID3D12DeviceVersion::Device10(device),
                    ResourceStateOrBarrierLayout::BarrierLayout(initial_layout),
                ) => device.CreateReservedResource2(
                    resource_desc,
                    initial_layout,
                    clear_value,
                    None,
                    None,
                    &mut result,
                ),
                (_, ResourceStateOrBarrierLayout::BarrierLayout(_)) => {
                    return Err(AllocationError::BarrierLayoutNeedsDevice10)
                }
                (device, ResourceStateOrBarrierLayout::ResourceState(initial_state)) => device
                    .CreateReservedResource(resource_desc, initial_state, clear_value, &mut result),
            }
        } {
            if e.code() == DXGI_ERROR_DEVICE_REMOVED {
                return Err(AllocationError::Internal(format!(
                    "ID3D12Device::CreateReservedResource DEVICE_REMOVED: {:?}",
                    unsafe { self.device.GetDeviceRemovedReason() }
                )));
            }
            return Err(AllocationError::Internal(format!(
                "ID3D12Device::CreateReservedResource failed: {e}"
            )));
        }

        Ok(result.expect("Allocation succeeded but no resource was returned?"))
    }

    /// Stomp path of [`Self::create_resource()`]: reserved resource, payload tiles mapped to
    /// sub-allocator memory, guard tiles mapped to the never-resident heap.
    fn create_resource_stomp(&mut self, desc: &ResourceCreateDesc<'_>) -> Result<Resource> {
        let settings = &self.stomp.as_ref().expect("stomp state exists").settings;
        let is_buffer = desc.resource_desc.Dimension == D3D12_RESOURCE_DIMENSION_BUFFER;
        // Textures get no guard tiles: no unbounded write path exists for them.
        let (head_guard, tail_guard) = if is_buffer {
            (settings.head_guard, settings.tail_guard)
        } else {
            (false, false)
        };

        let mut resource_desc = *desc.resource_desc;
        resource_desc.Alignment = 0;
        let (mut payload_tiles, offset) = if is_buffer {
            let (tiles, offset) = stomp_geometry(
                resource_desc.Width,
                tail_guard,
                head_guard,
                settings.payload_alignment,
            );
            resource_desc.Width =
                u64::from(u32::from(head_guard) + tiles + u32::from(tail_guard)) * TILE;
            (tiles, offset)
        } else {
            resource_desc.Layout = D3D12_TEXTURE_LAYOUT_64KB_UNDEFINED_SWIZZLE;
            (0, 0)
        };

        let resource = self.create_reserved(&resource_desc, desc)?;
        if !is_buffer {
            let mut num_subresource_tilings = 0;
            let mut subresource_tiling = D3D12_SUBRESOURCE_TILING::default();
            unsafe {
                self.device.GetResourceTiling(
                    &resource,
                    Some(&mut payload_tiles),
                    None,
                    None,
                    Some(&mut num_subresource_tilings),
                    0,
                    &mut subresource_tiling,
                )
            };
        }
        if payload_tiles == 0 {
            return Err(AllocationError::InvalidAllocationCreateDesc);
        }
        let layout = StompLayout {
            // Textures have no VA; asking would only raise a debug layer warning.
            resource_va: if is_buffer {
                unsafe { resource.GetGPUVirtualAddress() }
            } else {
                0
            },
            offset,
            payload_tiles,
            head_guard,
            tail_guard,
        };

        let allocation = self.allocate(&AllocationCreateDesc {
            name: desc.name,
            location: desc.memory_location,
            size: u64::from(payload_tiles) * TILE,
            alignment: TILE,
            resource_category: desc.resource_category,
        })?;

        let mapped = (|| {
            self.map_tiles(
                &resource,
                layout.head_tiles(),
                payload_tiles,
                &allocation.heap,
                (allocation.offset / TILE) as u32,
                false,
            );
            if head_guard || tail_guard {
                let guard_heap = self.memory_types[allocation.memory_type_index]
                    .guard_heap(&self.device)?
                    .clone();
                if head_guard {
                    self.map_tiles(&resource, 0, 1, &guard_heap, 0, true);
                }
                if tail_guard {
                    self.map_tiles(
                        &resource,
                        layout.head_tiles() + payload_tiles,
                        1,
                        &guard_heap,
                        0,
                        true,
                    );
                }
            }
            self.wait_for_mappings()
        })();
        if let Err(e) = mapped {
            drop(resource);
            self.free(allocation)?;
            return Err(e);
        }

        // Best effort: this is the name DRED reports on a page fault.
        let wide_name: Vec<u16> = desc.name.encode_utf16().chain(Some(0)).collect();
        let _ = unsafe { resource.SetName(PCWSTR::from_raw(wide_name.as_ptr())) };

        self.stomp.as_mut().expect("stomp state exists").guarded += 1;
        Ok(Resource {
            name: desc.name.into(),
            allocation: Some(allocation),
            resource: Some(resource),
            size: u64::from(payload_tiles) * TILE,
            memory_location: desc.memory_location,
            memory_type_index: None,
            stomp: Some(layout),
            offset,
        })
    }

    /// Remap every tile of a freed stomp resource to the never-resident heap and keep the
    /// resource alive, so stale descriptors and stale GPU VAs fault instead of reading reused
    /// memory.
    fn quarantine(
        &mut self,
        resource: ID3D12Resource,
        layout: StompLayout,
        memory_type_index: usize,
    ) -> Result<()> {
        let guard_heap = self.memory_types[memory_type_index]
            .guard_heap(&self.device)?
            .clone();
        self.map_tiles(&resource, 0, layout.total_tiles(), &guard_heap, 0, true);
        self.wait_for_mappings()?;
        self.stomp
            .as_mut()
            .expect("stomp state exists")
            .quarantine
            .push(resource);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        d3d12::{HeapCategory, ResourceCategory, ResourceType},
        MemoryLocation,
    };
    use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC};

    const BUFFER_DESC: D3D12_RESOURCE_DESC = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
        Alignment: 0,
        Width: 4000,
        Height: 1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: DXGI_FORMAT_UNKNOWN,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        Flags: D3D12_RESOURCE_FLAG_NONE,
    };

    fn desc(category: ResourceCategory) -> ResourceCreateDesc<'static> {
        ResourceCreateDesc {
            name: "test",
            memory_location: MemoryLocation::GpuOnly,
            resource_category: category,
            resource_desc: &BUFFER_DESC,
            castable_formats: &[],
            clear_value: None,
            initial_state_or_layout: ResourceStateOrBarrierLayout::ResourceState(
                D3D12_RESOURCE_STATE_COMMON,
            ),
            resource_type: &ResourceType::Placed,
            stomp: None,
        }
    }

    #[test]
    fn geometry_table() {
        // (size, tail, head, payload_alignment) -> (payload_tiles, offset)
        let cases = [
            ((4000, true, false, Some(8)), (1, 61536)),
            ((4000, true, false, None), (1, 0)),
            ((4000, true, true, Some(256)), (1, 65536 + 61440)),
            ((4000, false, true, Some(8)), (1, 65536)),
            ((65536, true, false, Some(8)), (1, 0)),
            ((65537, true, false, Some(8)), (2, 65528)),
            ((70000, true, false, Some(256)), (2, 60928)),
            ((1, true, false, Some(65536)), (1, 0)),
            ((1, true, false, Some(8)), (1, 65528)),
        ];
        for ((size, tail, head, alignment), expected) in cases {
            assert_eq!(
                stomp_geometry(size, tail, head, alignment),
                expected,
                "size {size} tail {tail} head {head} alignment {alignment:?}"
            );
        }
    }

    #[test]
    fn geometry_properties() {
        let mut rng = 1;
        for _ in 0..10_000 {
            let size = xorshift64(&mut rng) % (8 << 20) + 1;
            let alignment = [8, 16, 256, 4096, 65536][(xorshift64(&mut rng) % 5) as usize];
            let head = xorshift64(&mut rng) % 2 == 0;
            let (tiles, offset) = stomp_geometry(size, true, head, Some(alignment));
            let payload_end = u64::from(u32::from(head) + tiles) * TILE;
            assert_eq!(offset % alignment, 0);
            assert!(offset >= u64::from(head) * TILE);
            assert!(offset + size <= payload_end);
            assert!(payload_end - (offset + size) < alignment);

            let (tiles, offset) = stomp_geometry(size, true, head, None);
            assert_eq!(offset, u64::from(head) * TILE);
            assert!(offset + size <= u64::from(u32::from(head) + tiles) * TILE);
        }
    }

    fn draws(probability: f32, seed: u64, n: usize) -> Vec<bool> {
        let mut rng = seed | 1;
        let d = desc(ResourceCategory::Buffer);
        (0..n)
            .map(|_| stomp_decide(StompMode::Random { probability }, None, &mut rng, &d))
            .collect()
    }

    #[test]
    fn random_bounds() {
        assert!(draws(0.0, 7, 10_000).iter().all(|&b| !b));
        assert!(draws(1.0, 7, 10_000).iter().all(|&b| b));
        let hits = draws(0.5, 7, 100_000).iter().filter(|&&b| b).count();
        assert!((45_000..=55_000).contains(&hits), "{hits}");
    }

    #[test]
    fn random_is_deterministic() {
        assert_eq!(draws(0.5, 42, 1000), draws(0.5, 42, 1000));
    }

    #[test]
    fn xorshift_never_sticks_at_zero() {
        // Seed 0 is what `Allocator::new` remaps with `| 1`.
        let seed: u64 = 0;
        let mut state = seed | 1;
        for _ in 0..1000 {
            xorshift64(&mut state);
            assert_ne!(state, 0);
        }
    }

    #[test]
    fn decision_matrix() {
        let mut rng = 1;
        let buffer = desc(ResourceCategory::Buffer);
        let texture = desc(ResourceCategory::OtherTexture);
        let filter = StompMode::Filter(|d| d.resource_category == ResourceCategory::Buffer);

        assert!(stomp_decide(StompMode::All, None, &mut rng, &buffer));
        assert!(!stomp_decide(
            StompMode::All,
            Some(false),
            &mut rng,
            &buffer
        ));
        assert!(!stomp_decide(StompMode::OptIn, None, &mut rng, &buffer));
        assert!(stomp_decide(
            StompMode::OptIn,
            Some(true),
            &mut rng,
            &buffer
        ));
        assert!(stomp_decide(filter, None, &mut rng, &buffer));
        assert!(!stomp_decide(filter, None, &mut rng, &texture));
        assert!(stomp_decide(filter, Some(true), &mut rng, &texture));
        assert!(stomp_decide(
            StompMode::Random { probability: 0.0 },
            Some(true),
            &mut rng,
            &buffer
        ));
    }

    #[test]
    fn settings_validation() {
        let invalid = |r: Result<()>| matches!(r, Err(AllocationError::InvalidStompSettings(_)));
        // `StompSettings::new` defaults.
        assert!(validate_stomp_settings(StompMode::All, true, false, None).is_ok());
        assert!(validate_stomp_settings(StompMode::All, true, false, Some(256)).is_ok());
        assert!(validate_stomp_settings(StompMode::All, true, false, Some(65536)).is_ok());
        for probability in [1.5, -0.1, f32::NAN] {
            assert!(invalid(validate_stomp_settings(
                StompMode::Random { probability },
                true,
                false,
                None
            )));
        }
        assert!(invalid(validate_stomp_settings(
            StompMode::All,
            false,
            false,
            None
        )));
        for alignment in [0, 3, 131072] {
            assert!(invalid(validate_stomp_settings(
                StompMode::All,
                true,
                false,
                Some(alignment)
            )));
        }
    }

    #[test]
    fn heap_flags_mapping() {
        assert_eq!(heap_flags(HeapCategory::All), D3D12_HEAP_FLAG_NONE);
        assert_eq!(
            heap_flags(HeapCategory::Buffer),
            D3D12_HEAP_FLAG_ALLOW_ONLY_BUFFERS
        );
        assert_eq!(
            heap_flags(HeapCategory::RtvDsvTexture),
            D3D12_HEAP_FLAG_ALLOW_ONLY_RT_DS_TEXTURES
        );
        assert_eq!(
            heap_flags(HeapCategory::OtherTexture),
            D3D12_HEAP_FLAG_ALLOW_ONLY_NON_RT_DS_TEXTURES
        );
    }
}
