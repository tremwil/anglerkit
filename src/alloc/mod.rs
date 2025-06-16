//! Implements read-write-execute memory allocators.

use core::{alloc::Layout, ops::Range, ptr::NonNull};

#[cfg(feature = "std")]
mod region;

pub mod arc;
pub mod block;
pub mod boxed;

/// Allocation error describing the different failure scenarios of a [`NearAllocator`].
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocError {
    /// The allocator ran out of memory.
    #[error("Out of memory")]
    OutOfMemory,
    /// The allocation address restrictions (e.g. within a range of addresses) could not
    /// be satisfied.
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

/// Types which can be converted to a [`*mut T`](pointer).
///
/// Unlike [`Into<*mut T>`], this takes `self` by reference.
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
    ///
    /// # Safety
    /// `layout.size()` must be nonzero.
    unsafe fn alloc_within(
        &self,
        range: Range<usize>,
        layout: Layout,
    ) -> Result<Self::Ptr, AllocError>;

    /// Allocate executable memory according to the provided [`Layout`].
    ///
    /// The default implementation uses [`NearAllocator::alloc_within`] with the entire
    /// address space for `range`.
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
    /// The default implementation uses [`NearAllocator::alloc_within`] internally.
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
        // maximum address at which the end of layout could point
        let max_addr = address.saturating_add(displacement);

        unsafe { self.alloc_within(min_addr..max_addr, layout) }
    }

    /// Free memory previously allocated through this allocator.
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
        range: Range<usize>,
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
        range: Range<usize>,
        layout: Layout,
    ) -> Result<Self::Ptr, AllocError> {
        unsafe { (**self).alloc_within(range, layout) }
    }

    unsafe fn free(&self, ptr: Self::Ptr, layout: Layout) {
        unsafe { (**self).free(ptr, layout) };
    }
}

/// The global [`NearAllocator`].
pub struct GlobalNearAlloc;

enum AllocConstraint {
    None,
    Near { address: usize, displacement: usize },
    Within(Range<usize>),
}

/// Wrapper around a [`NearAllocator`] with pre-applied allocation constraints, e.g.
/// range parameters to [`NearAllocator::alloc_within`]
pub struct Constrained<A: NearAllocator> {
    allocator: A,
    constraint: AllocConstraint,
}

impl<A: NearAllocator> From<A> for Constrained<A> {
    fn from(value: A) -> Self {
        Self {
            allocator: value,
            constraint: AllocConstraint::None,
        }
    }
}

impl<A: NearAllocator> Constrained<A> {
    /// Create a constrained [`NearAllocator`] without any actual constraints.
    ///
    /// Calls to [`Self::alloc`] with defer to [`NearAllocator::alloc`].
    pub fn new(allocator: A) -> Self {
        Self {
            allocator,
            constraint: AllocConstraint::None,
        }
    }

    /// Consumes self, returning the wrapped [`NearAllocator`].
    pub fn into_inner(self) -> A {
        self.allocator
    }

    /// Create a constrained [`NearAllocator`] which guarantees that the memory
    /// of allocated values will lie within `displacement` bytes of `address`.
    ///
    /// Calls to [`Self::alloc`] with defer to [`NearAllocator::alloc_near`].
    pub fn new_near(allocator: A, address: usize, displacement: usize) -> Self {
        Self {
            allocator,
            constraint: AllocConstraint::Near {
                address,
                displacement,
            },
        }
    }

    /// Create a constrained [`NearAllocator`] which guarantees that the memory
    /// of allocated values will lie within `range`
    ///
    /// Calls to [`Self::alloc`] with defer to [`NearAllocator::alloc_within`].
    pub fn new_within(allocator: A, range: Range<usize>) -> Self {
        Self {
            allocator,
            constraint: AllocConstraint::Within(range),
        }
    }

    /// Attempt to allocate memory from the wrapped [`NearAllocator`] given the
    /// constraints.
    ///
    /// # Safety
    /// `layout.size()` must be nonzero.
    pub unsafe fn alloc(&self, layout: Layout) -> Result<A::Ptr, AllocError> {
        unsafe {
            match &self.constraint {
                AllocConstraint::None => self.allocator.alloc(layout),
                AllocConstraint::Near {
                    address,
                    displacement,
                } => self.allocator.alloc_near(*address, *displacement, layout),
                AllocConstraint::Within(r) => self.allocator.alloc_within(r.clone(), layout),
            }
        }
    }

    /// Frees previously allocated memory.
    ///
    /// # Safety
    /// - The pointer must have been previously allocated through [`Self::alloc`] with
    ///   the same layout.
    pub unsafe fn free(&self, ptr: A::Ptr, layout: Layout) {
        unsafe {
            self.allocator.free(ptr, layout);
        }
    }
}

/// Trait alias for a [`NearAllocator`] which is [`Clone`] along with its pointer type.
///
/// This is often a requirement for data structures which share the allocator, such a
/// reference-counter pointers.
pub unsafe trait CloneableNearAlloc: NearAllocator + Clone {
    /// Clone this [`NearAllocator`]'s pointer type.
    fn clone_ptr(ptr: &Self::Ptr) -> Self::Ptr;
}

unsafe impl<A: NearAllocator> CloneableNearAlloc for A
where
    A: Clone,
    <A as NearAllocator>::Ptr: Clone,
{
    fn clone_ptr(ptr: &Self::Ptr) -> Self::Ptr {
        ptr.clone()
    }
}
