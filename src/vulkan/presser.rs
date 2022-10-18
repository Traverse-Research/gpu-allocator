use super::Allocation;
use core::convert::TryFrom;

impl Allocation {
    /// Borrow the CPU-mapped memory that backs this allocation as a [`presser::Slab`], which you can then
    /// use to safely copy data into the raw, potentially-uninitialized buffer.
    ///
    /// Returns [`None`] if `self.mapped_ptr()` is `None`, or if `self.size()` is > `isize::MAX` because
    /// this could lead to undefined behavior.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// #[repr(C, align(16))]
    /// #[derive(Clone, Copy)]
    /// struct MyGpuVector {
    ///     x: f32,
    ///     y: f32,
    ///     z: f32,
    /// }
    ///
    /// // Create some data to be sent to the GPU. Note this must be formatted correctly in terms of
    /// // alignment of individual items and etc, as usual.
    /// let my_gpu_data: &[MyGpuVector] = get_vertex_data();
    ///
    /// // Get a `presser::Slab` from our gpu_allocator::Allocation
    /// let mut alloc_slab = my_allocation.as_mapped_slab().unwrap();
    ///
    /// // depending on the type of data you're copying, your vulkan device may have a minimum
    /// // alignment requirement for that data
    /// let min_gpu_align = my_vulkan_device_specifications.min_alignment_thing;
    ///
    /// let copy_record = presser::copy_from_slice_to_offset_with_align(
    ///     my_gpu_data,
    ///     &mut alloc_slab,
    ///     0, // start as close to the beginning of the allocation as possible
    ///     min_gpu_align,
    /// );
    ///
    /// // the data may not have actually been copied starting at the requested start offset
    /// // depending on the alignment of the underlying allocation, as well as the alignment requirements of
    /// // `MyGpuVector` and the `min_gpu_align` we passed in
    /// let actual_data_start_offset = copy_record.copy_start_offset;
    /// ```
    ///
    /// # Safety
    ///
    /// This is technically not fully safe because we can't validate that the
    /// GPU is not using the data in the buffer while `self` is borrowed, however trying
    /// to validate this statically is really hard and the community has basically decided
    /// that just calling stuff like this is fine. So, as would always be the case, ensure the GPU
    /// is not using the data in `self` before calling this function.
    pub fn as_mapped_slab(&mut self) -> Option<MappedAllocationSlab<'_>> {
        let mapped_ptr = self.mapped_ptr()?.cast().as_ptr();
        // size > isize::MAX is disallowed by `Slab` for safety reasons
        let size = isize::try_from(self.size()).ok()?;
        // this will always succeed since size can only be positive and < isize::MAX
        let size = size as usize;

        Some(MappedAllocationSlab {
            _borrowed_alloc: self,
            mapped_ptr,
            size,
        })
    }
}

/// A wrapper struct over a borrowed [`Allocation`] that implements [`presser::Slab`].
///
/// This type should be acquired by calling [`Allocation::as_mapped_slab`].
pub struct MappedAllocationSlab<'a> {
    _borrowed_alloc: &'a mut Allocation,
    mapped_ptr: *mut u8,
    size: usize,
}

// SAFETY: See the safety comment of Allocation::as_mapped_slab above.
unsafe impl<'a> presser::Slab for MappedAllocationSlab<'a> {
    fn base_ptr(&self) -> *const u8 {
        self.mapped_ptr
    }

    fn base_ptr_mut(&mut self) -> *mut u8 {
        self.mapped_ptr
    }

    fn size(&self) -> usize {
        self.size
    }
}
