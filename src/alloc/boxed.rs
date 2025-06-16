use core::{
    alloc::Layout,
    borrow::{Borrow, BorrowMut},
    fmt::{Debug, Display},
    hash::Hash,
    marker::PhantomData,
    mem::MaybeUninit,
    ops::{Deref, DerefMut},
};

use super::{AllocError, Constrained, NearAllocator};
use crate::alloc::AsMutPtr;

/// An owned pointer to a value allocated using a [`Slab`].
///
/// This has similar semantics to the standard library's `Box` type. However, it cannot
/// store dynamically-sized types due to slab allocators only being able to allocate
/// values of a specific sized type.
pub struct Box<T, A: NearAllocator> {
    // Required for ZST support
    //
    // Sadly, this breaks niche layout optimizations. The other way is to add a `dangling()`
    // function in the `AsMutPtr` trait, which requires pointers with a Drop impl to store
    // initialization state.
    ptr: MaybeUninit<A::Ptr>,
    allocator: A,
    phantom: PhantomData<T>,
}

impl<T, A: NearAllocator> Box<T, A> {
    /// Try to allocate `value` in a specific near allocator.
    ///
    /// Allocation constraints are specified in the [`Bound`] wrapper.
    ///
    /// # Errors
    /// Fails if `allocator` fails to allocate memory for `value`.
    pub fn try_new(value: T, allocator: Constrained<A>) -> Result<Self, AllocError> {
        // If T is a ZST, calling `alloc` is unsound. Instead, don't initialize the pointer.
        if size_of::<T>() == 0 {
            return Ok(Self {
                ptr: MaybeUninit::uninit(),
                allocator: allocator.into_inner(),
                phantom: PhantomData,
            });
        }

        let ptr = unsafe { allocator.alloc(Layout::new::<T>())? };
        // SAFETY: we have exclusive access to the memory pointed at by `ptr`
        unsafe {
            ptr.as_mut_ptr().cast::<T>().write(value);
        };
        Ok(Self {
            ptr: MaybeUninit::new(ptr),
            allocator: allocator.into_inner(),
            phantom: PhantomData,
        })
    }

    /// Try to allocate `value` in a specific near allocator.
    ///
    /// Allocation constraints are specified in the [`Bound`] wrapper.
    ///
    /// # Panics
    /// If the `allocator` fails to allocate memory for `value`.
    pub fn new(value: T, allocator: Constrained<A>) -> Self {
        Self::try_new(value, allocator).unwrap()
    }

    /// Get a const pointer to the allocated memory.
    pub fn as_ptr(&self) -> *const T {
        if size_of::<T>() == 0 {
            core::ptr::dangling()
        }
        else {
            // SAFETY: `self.ptr` is initialized.
            unsafe { self.ptr.assume_init_ref().as_mut_ptr().cast() }
        }
    }

    pub fn as_mut_ptr(&mut self) -> *mut T {
        self.as_ptr().cast_mut()
    }
}

impl<T, A: NearAllocator> Drop for Box<T, A> {
    fn drop(&mut self) {
        // Nothing to do here if T is a ZST
        if size_of::<T>() == 0 {
            return;
        }

        // SAFETY:
        // - `self.ptr` is initialized
        // - drop check ensures the lifetime of the allocator is not over
        // - `ptr` is currently allocated in `self.allocator`
        unsafe {
            let ptr = self.ptr.assume_init_read();
            core::ptr::drop_in_place(ptr.as_mut_ptr().cast::<T>());
            self.allocator.free(ptr, Layout::new::<T>());
        };
    }
}

impl<T, A: NearAllocator> AsRef<T> for Box<T, A> {
    fn as_ref(&self) -> &T {
        // SAFETY: `self`'s lifetime is shorter than that of `ptr`'s allocator
        unsafe { &*self.as_ptr() }
    }
}

impl<T, A: NearAllocator> AsMut<T> for Box<T, A> {
    fn as_mut(&mut self) -> &mut T {
        // SAFETY:
        // - `self`'s lifetime is shorter than that of `ptr`'s allocator
        // - we have exclusive access to `self`
        unsafe { &mut *self.as_mut_ptr() }
    }
}

impl<T, A: NearAllocator + Clone> Borrow<T> for Box<T, A> {
    fn borrow(&self) -> &T {
        self.as_ref()
    }
}

impl<T, A: NearAllocator + Clone> BorrowMut<T> for Box<T, A> {
    fn borrow_mut(&mut self) -> &mut T {
        self.as_mut()
    }
}

impl<T, A: NearAllocator> Deref for Box<T, A> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.as_ref()
    }
}

impl<T, A: NearAllocator> DerefMut for Box<T, A> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.as_mut()
    }
}

impl<T: Debug, A: NearAllocator> Debug for Box<T, A>
where
    A: Debug,
    A::Ptr: Debug,
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Box")
            .field("ptr", &self.ptr)
            .field("allocator", &self.allocator)
            .finish()
    }
}

impl<T: Display, A: NearAllocator> Display for Box<T, A> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.as_ref().fmt(f)
    }
}

impl<T: PartialEq, A: NearAllocator> PartialEq for Box<T, A> {
    fn eq(&self, other: &Self) -> bool {
        self.as_ref().eq(other.as_ref())
    }
}

impl<T: Eq, A: NearAllocator> Eq for Box<T, A> {}

impl<T: PartialOrd, A: NearAllocator> PartialOrd for Box<T, A> {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        self.as_ref().partial_cmp(other.as_ref())
    }

    fn ge(&self, other: &Self) -> bool {
        self.as_ref().ge(other.as_ref())
    }

    fn gt(&self, other: &Self) -> bool {
        self.as_ref().gt(other.as_ref())
    }

    fn le(&self, other: &Self) -> bool {
        self.as_ref().le(other.as_ref())
    }

    fn lt(&self, other: &Self) -> bool {
        self.as_ref().lt(other.as_ref())
    }
}

impl<T: Ord, A: NearAllocator> Ord for Box<T, A> {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.as_ref().cmp(other.as_ref())
    }
}

impl<T: Hash, A: NearAllocator> Hash for Box<T, A> {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.as_ref().hash(state);
    }
}
