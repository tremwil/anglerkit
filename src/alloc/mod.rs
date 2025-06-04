//! Implements typed read-write-execute memory allocators.

mod atomic_slab;
mod slab_pool;

/// Allocation error.
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    #[error("Out of memory")]
    OutOfMemory,
    #[error("No suitable region found for near allocation")]
    NoSuitableRegion,
}

/// Types that wrap a raw pointer to a value of `T`.
pub trait AsPtr<T> {
    fn as_ptr(&self) -> *const T;
    fn as_ptr_mut(&mut self) -> *mut T;
}

/// Raw executable memory allocator that allows allocating arbitrary amounts of
/// read-write-executable memory near an address.
pub unsafe trait NearAllocator {
    /// Allocate at least `size` bytes of executable memory. All bytes of
    /// allocated storage will be at most `displacement` bytes away from
    /// `address`.
    fn alloc_near(
        &self,
        address: usize,
        displacement: usize,
        size: usize,
    ) -> Result<*mut [u8], Error>;

    /// Free a pointer returned by [`alloc_near`].
    ///
    /// # Safety
    /// - The pointer must have been previously allocated through
    ///   [`NearAllocator::alloc_near`].
    unsafe fn free(&self, ptr: *mut u8);
}

unsafe impl<'a, A: NearAllocator> NearAllocator for &'a A {
    fn alloc_near(
        &self,
        address: usize,
        displacement: usize,
        size: usize,
    ) -> Result<*mut [u8], Error> {
        (**self).alloc_near(address, displacement, size)
    }

    unsafe fn free(&self, ptr: *mut u8) {
        (**self).free(ptr);
    }
}

unsafe impl<A: NearAllocator> NearAllocator for crate::liballoc::boxed::Box<A> {
    fn alloc_near(
        &self,
        address: usize,
        displacement: usize,
        size: usize,
    ) -> Result<*mut [u8], Error> {
        (**self).alloc_near(address, displacement, size)
    }

    unsafe fn free(&self, ptr: *mut u8) {
        (**self).free(ptr);
    }
}

/// Typed executable memory allocator suitable for storing thunks/trampolines.
///
/// # Safety
/// The memory returned by [`ThunkAllocator::alloc_near`] must be
/// read-write-executable.
pub unsafe trait ThunkAllocator<T> {
    /// The pointer type returned by the allocator.
    type Ptr: AsPtr<T>;

    /// Allocate space for a value of T in executable memory. All bytes of
    /// allocated storage will be at most `displacement` bytes away from
    /// `address`.
    fn alloc_near(&self, address: usize, displacement: usize) -> Result<Self::Ptr, Error>;

    /// Free a pointer returned by [`alloc_near`].
    ///
    /// # Safety
    /// - The pointer must have been previously allocated through
    ///   [`ThunkAllocator::alloc_near`].
    unsafe fn free(&self, ptr: Self::Ptr);
}

unsafe impl<'a, T, A: ThunkAllocator<T>> ThunkAllocator<T> for &'a A {
    type Ptr = A::Ptr;

    fn alloc_near(&self, address: usize, displacement: usize) -> Result<Self::Ptr, Error> {
        (**self).alloc_near(address, displacement)
    }

    unsafe fn free(&self, ptr: Self::Ptr) {
        (**self).free(ptr);
    }
}

unsafe impl<T, A: ThunkAllocator<T>> ThunkAllocator<T> for crate::liballoc::boxed::Box<A> {
    type Ptr = A::Ptr;

    fn alloc_near(&self, address: usize, displacement: usize) -> Result<Self::Ptr, Error> {
        (**self).alloc_near(address, displacement)
    }

    unsafe fn free(&self, ptr: Self::Ptr) {
        (**self).free(ptr);
    }
}
