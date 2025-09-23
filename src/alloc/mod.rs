//! Implements read-write-execute memory allocators.

use core::{alloc::Layout, ops::Range, ptr::NonNull};

#[cfg(feature = "std")]
mod region;

pub mod arc;

#[cfg(feature = "block_alloc")]
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

    /// Free unused memory pools held by the allocator.
    ///
    /// For performance reasons, it is reasonnable for an implementation to keep virtual
    /// memory blocks obtained from the OS even if they are currently unused, since
    /// future [`alloc_near`](NearAllocator::alloc_near) or
    /// [`alloc_within`](NearAllocator::alloc_within) calls are likely to require memory
    /// around the same region. When this method is called, the allocator should free
    /// these blocks.
    ///
    /// The default implementation does nothing.
    fn garbage_collect(&self) {}
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
///
/// When the `default_alloc` feature is enabled, this is implemented using a static
/// [`RegionNearAlloc`](region::RegionNearAlloc) wrapped in a
/// [`BlockNearAlloc`](block::BlockNearAlloc). Otherwise, it defers to the
/// [`NearAllocator`] instance provided to the [`global_near_alloc!`] macro.
pub struct GlobalNearAlloc;

#[cfg(feature = "default_alloc")]
mod default_alloc {
    use core::{alloc::Layout, ops::Range, ptr::NonNull};

    use crate::alloc::{
        block::BlockNearAlloc, region::RegionNearAlloc, AllocError, GlobalNearAlloc, NearAllocator,
    };

    #[cfg(feature = "default_alloc")]
    type Mutex = parking_lot::RawMutex;
    #[cfg(not(feature = "default_alloc"))]
    type Mutex = spin::Mutex<()>;

    static GLOBAL_NEAR_ALLOC: BlockNearAlloc<Mutex, RegionNearAlloc> =
        BlockNearAlloc::new(RegionNearAlloc);

    unsafe impl NearAllocator for GlobalNearAlloc {
        type Ptr = *mut u8;

        unsafe fn alloc(&self, layout: Layout) -> Result<Self::Ptr, AllocError> {
            unsafe { GLOBAL_NEAR_ALLOC.alloc(layout) }
        }

        unsafe fn alloc_within(
            &self,
            range: Range<usize>,
            layout: Layout,
        ) -> Result<Self::Ptr, AllocError> {
            unsafe { GLOBAL_NEAR_ALLOC.alloc_within(range, layout) }
        }

        unsafe fn alloc_near(
            &self,
            address: usize,
            displacement: usize,
            layout: Layout,
        ) -> Result<Self::Ptr, AllocError> {
            unsafe { GLOBAL_NEAR_ALLOC.alloc_near(address, displacement, layout) }
        }

        unsafe fn free(&self, ptr: Self::Ptr, layout: Layout) {
            unsafe {
                GLOBAL_NEAR_ALLOC.free(ptr, layout);
            }
        }

        fn garbage_collect(&self) {
            GLOBAL_NEAR_ALLOC.garbage_collect();
        }
    }
}

#[cfg(not(feature = "default_alloc"))]
mod custom_alloc {
    use core::{alloc::Layout, ops::Range};

    use crate::alloc::{AllocError, GlobalNearAlloc, NearAllocator};

    type DynNearAlloc = &'static (dyn NearAllocator<Ptr = *mut u8> + Sync);

    extern "Rust" {
        fn anglerkit_v0_global_near_alloc() -> DynNearAlloc;
    }

    fn global_alloc() -> DynNearAlloc {
        unsafe { anglerkit_v0_global_near_alloc() }
    }

    unsafe impl NearAllocator for GlobalNearAlloc {
        type Ptr = *mut u8;

        unsafe fn alloc(&self, layout: Layout) -> Result<Self::Ptr, AllocError> {
            unsafe { global_alloc().alloc(layout) }
        }

        unsafe fn alloc_within(
            &self,
            range: Range<usize>,
            layout: Layout,
        ) -> Result<Self::Ptr, AllocError> {
            unsafe { global_alloc().alloc_within(range, layout) }
        }

        unsafe fn alloc_near(
            &self,
            address: usize,
            displacement: usize,
            layout: Layout,
        ) -> Result<Self::Ptr, AllocError> {
            unsafe { global_alloc().alloc_near(address, displacement, layout) }
        }

        unsafe fn free(&self, ptr: Self::Ptr, layout: Layout) {
            unsafe { global_alloc().free(ptr, layout) }
        }

        fn garbage_collect(&self) {
            global_alloc().garbage_collect()
        }
    }
}

/// Specify the [`NearAllocator<Ptr = *mut u8>`] implementation that [`GlobalNearAlloc`]
/// will defer to.
///
/// The macro accepts a path to a static variable or an unsafe block resolving to a
/// `&'static (dyn NearAllocator<Ptr = *mut u8> + Sync)`:
///
/// ```ignore
/// static GLOBAL_NEAR: MyNearAlloc = MyNearAlloc::new();
/// global_jit_alloc!(GLOBAL_NEAR);
/// ```
///
/// ```ignore
/// use std::sync::OnceLock;
///
/// global_near_alloc!(unsafe {
///     static WRAPPED: OnceLock<MyNearAlloc> = OnceLock::new();
///     WRAPPED.get_or_init(|| MyNearAlloc::new())
/// });
/// ```
///
/// # Safety
/// The block form must be marked with `unsafe` as sometimes returning a different
/// instance is unsound, and you are responsible to make sure this doesn't happen.
#[cfg(any(doc, not(feature = "default_alloc")))]
#[macro_export]
macro_rules! global_near_alloc {
    ($static_var:path) => {
        #[unsafe(no_mangle)]
        extern "Rust" fn anglerkit_v0_global_near_alloc(
        ) -> &'static (dyn NearAllocator<Ptr = *mut u8> + Sync) {
            $static_var
        }
    };
    (unsafe $provider:block) => {
        #[unsafe(no_mangle)]
        extern "Rust" fn anglerkit_v0_global_near_alloc(
        ) -> &'static (dyn NearAllocator<Ptr = *mut u8> + Sync) {
            unsafe { $static_var }
        }
    };
}

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
