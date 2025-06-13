//! Implements read-write-execute memory allocators.

use core::{alloc::Layout, ptr::NonNull};

#[cfg(not(feature = "no_std"))]
mod region;

pub mod block;

/// Allocation error describing the different failure scenarios of a [`NearAllocator`].
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocError {
    /// The allocator ran out of memory.
    #[error("Out of memory")]
    OutOfMemory,
    /// The allocation address restrictions (e.g. within a range of addresses) could be
    /// satisfied.
    #[error("No suitable region found satisfying address restrictions")]
    NoSuitableRegion,
    /// The layout being allocated is not supported by the allocator (e.g. the size or
    /// alignment is too large).
    #[error("Cannot allocate the provided layout")]
    UnsupportedLayout,
    /// An operating system call (e.g. mmap/VirtualAlloc) that was required for
    /// allocation failed.
    #[error("A call to the operating system failed during allocation")]
    OsError,
    /// An opaque implementation-specific error occured. Avoid using this error variant
    /// unless none of the others describe the error.
    #[error("Internal allocator error")]
    Internal,
}

/// Types which can be converted to a *mut T through an immutable reference.
pub trait AsMutPtr<T> {
    fn as_mut_ptr(&self) -> *mut T;
}

impl<T> AsMutPtr<T> for *mut T {
    fn as_mut_ptr(&self) -> *mut T {
        *self
    }
}

impl<T> AsMutPtr<T> for NonNull<T> {
    fn as_mut_ptr(&self) -> *mut T {
        self.as_ptr()
    }
}

/// Allocator that allows allocating read-write-executable memory conforming to a
/// specific [`Layout`] and within a particular address range.
///
/// # Safety
/// Implementors must satisfy the same [safety
/// guarantees](core::alloc::Allocator#safety) as that of the experimental
/// [`Allocator`](core::alloc::Allocator) trait.
pub unsafe trait NearAllocator {
    type Ptr: AsMutPtr<u8>;

    /// Allocate executable memory according to the provided [`Layout`]. All bytes of
    /// allocated storage are guaranteed to lie within `range`.
    /// `address`.
    ///
    /// # Safety
    /// `layout.size()` must be nonzero.
    unsafe fn alloc_within(
        &self,
        range: core::ops::Range<usize>,
        layout: Layout,
    ) -> Result<Self::Ptr, AllocError>;

    /// Allocate executable memory according to the provided [`Layout`].
    ///
    /// # Safety
    /// `layout.size()` must be nonzero.
    unsafe fn alloc(&self, layout: Layout) -> Result<Self::Ptr, AllocError> {
        unsafe { self.alloc_within(0..usize::MAX, layout) }
    }

    /// Allocate executable memory according to the provided [`Layout`]. All bytes of
    /// allocated storage will be at most `displacement` bytes away from
    /// `address`.
    ///
    /// # Safety
    /// `layout.size()` must be nonzero.
    unsafe fn alloc_near(
        &self,
        address: usize,
        displacement: usize,
        layout: Layout,
    ) -> Result<Self::Ptr, AllocError> {
        // minimum address at which the layout could be allocated
        let min_addr = address.saturating_sub(displacement);
        let min_addr = min_addr
            .checked_next_multiple_of(layout.align())
            .ok_or(AllocError::NoSuitableRegion)?;

        // maximum address at which the end of layout could be allocated
        let max_addr = address.saturating_add(displacement) & (layout.align() - 1);

        unsafe { self.alloc_within(min_addr..max_addr, layout) }
    }

    /// Free a pointer returned by [`alloc_near`].
    ///
    /// # Safety
    /// - The pointer must have been previously allocated through
    ///   [`alloc`](NearAllocator::alloc), [`alloc_within`](NearAllocator::alloc_within)
    ///   or [`alloc_near`](NearAllocator::alloc_near) with the same `layout`.
    unsafe fn free(&self, ptr: Self::Ptr, layout: Layout);
}

unsafe impl<A: NearAllocator> NearAllocator for &A {
    type Ptr = A::Ptr;

    unsafe fn alloc(&self, layout: Layout) -> Result<Self::Ptr, AllocError> {
        unsafe { (**self).alloc(layout) }
    }

    unsafe fn alloc_near(
        &self,
        address: usize,
        displacement: usize,
        layout: Layout,
    ) -> Result<Self::Ptr, AllocError> {
        unsafe { (**self).alloc_near(address, displacement, layout) }
    }

    unsafe fn alloc_within(
        &self,
        range: core::ops::Range<usize>,
        layout: Layout,
    ) -> Result<Self::Ptr, AllocError> {
        unsafe { (**self).alloc_within(range, layout) }
    }

    unsafe fn free(&self, ptr: Self::Ptr, layout: Layout) {
        unsafe { (**self).free(ptr, layout) };
    }
}

unsafe impl<A: NearAllocator> NearAllocator for crate::liballoc::boxed::Box<A> {
    type Ptr = A::Ptr;

    unsafe fn alloc(&self, layout: Layout) -> Result<Self::Ptr, AllocError> {
        unsafe { (**self).alloc(layout) }
    }

    unsafe fn alloc_near(
        &self,
        address: usize,
        displacement: usize,
        layout: Layout,
    ) -> Result<Self::Ptr, AllocError> {
        unsafe { (**self).alloc_near(address, displacement, layout) }
    }

    unsafe fn alloc_within(
        &self,
        range: core::ops::Range<usize>,
        layout: Layout,
    ) -> Result<Self::Ptr, AllocError> {
        unsafe { (**self).alloc_within(range, layout) }
    }

    unsafe fn free(&self, ptr: Self::Ptr, layout: Layout) {
        unsafe { (**self).free(ptr, layout) };
    }
}
