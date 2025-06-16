use core::{
    alloc::Layout,
    borrow::Borrow,
    fmt::{Debug, Display},
    hash::Hash,
    marker::PhantomData,
    mem::ManuallyDrop,
    ops::Deref,
    sync::atomic::{AtomicUsize, Ordering::Relaxed},
};

use super::{AllocError, CloneableNearAlloc, Constrained};
use crate::alloc::{AsMutPtr, NearAllocator};

pub struct ArcData<T> {
    value: T,
    refcnt: AtomicUsize,
}

/// Atomically reference counted pointer to data allocated using a
/// [`CloneableNearAlloc`].
///
/// This is similar to the standard library's `Arc` type, except that it does not
/// support weak references.
pub struct Arc<T, A: CloneableNearAlloc> {
    ptr: ManuallyDrop<<A as NearAllocator>::Ptr>,
    allocator: A,
    phantom: PhantomData<T>,
}

impl<T, A: CloneableNearAlloc> Arc<T, A> {
    /// Try to allocate `value` in a specific near allocator.
    ///
    /// Allocation constraints are specified in the [`Bound`] wrapper.
    ///
    /// # Errors
    /// Fails if `allocator` fails to allocate memory.
    pub fn try_new(value: T, allocator: Constrained<A>) -> Result<Self, AllocError> {
        // SAFETY: layout has nonzero size
        let ptr = unsafe { allocator.alloc(Layout::new::<ArcData<T>>())? };
        unsafe {
            ptr.as_mut_ptr().cast::<ArcData<T>>().write(ArcData {
                value,
                refcnt: AtomicUsize::new(1),
            });
        };
        Ok(Self {
            ptr: ManuallyDrop::new(ptr),
            allocator: allocator.into_inner(),
            phantom: PhantomData,
        })
    }

    /// Try to allocate `value` in a specific near allocator.
    ///
    /// Allocation constraints are specified in the [`Bound`] wrapper.
    ///
    /// # Panics
    /// If `allocator` fails to allocate memory.
    pub fn new(value: T, allocator: Constrained<A>) -> Self {
        Self::try_new(value, allocator).unwrap()
    }

    fn data(&self) -> *mut ArcData<T> {
        self.ptr.as_mut_ptr().cast()
    }
}

impl<T, A: CloneableNearAlloc> Drop for Arc<T, A> {
    fn drop(&mut self) {
        let data_ptr = self.data();
        unsafe {
            // SAFETY: getting an immutable reference to the shared data pointer
            let data: &ArcData<T> = &*data_ptr;

            // Note that in this body, the ref count as hit zero and thus there are no other
            // references to the shared data pointer.
            if data.refcnt.fetch_sub(1, Relaxed) == 1 {
                // SAFETY: data_ptr.value is never going to be accessed again
                core::ptr::drop_in_place(&raw mut (*data_ptr).value);
                // SAFETY: self.ptr has not been dropped yet
                let ptr = (&raw const self.ptr).cast::<A::Ptr>().read();
                // SAFETY: ptr has been allocated using `self.allocator` with this layout
                self.allocator.free(ptr, Layout::new::<ArcData<T>>());
            }
        };
    }
}

impl<T, A: CloneableNearAlloc> AsRef<T> for Arc<T, A> {
    fn as_ref(&self) -> &T {
        unsafe { &(&*self.data()).value }
    }
}

impl<T, A: CloneableNearAlloc> Borrow<T> for Arc<T, A> {
    fn borrow(&self) -> &T {
        self.as_ref()
    }
}

impl<T, A: CloneableNearAlloc> Deref for Arc<T, A> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.as_ref()
    }
}

impl<T, A: CloneableNearAlloc> Clone for Arc<T, A> {
    fn clone(&self) -> Self {
        unsafe { (&*self.data()).refcnt.fetch_add(1, Relaxed) };
        Self {
            ptr: ManuallyDrop::new(A::clone_ptr(self.ptr.borrow())),
            allocator: self.allocator.clone(),
            phantom: PhantomData,
        }
    }
}

impl<T: Debug, A: CloneableNearAlloc> Debug for Arc<T, A>
where
    A: Debug,
    A::Ptr: Debug,
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Arc")
            .field("ptr", self.ptr.borrow())
            .field("allocator", &self.allocator)
            .finish()
    }
}

impl<T: Display, A: CloneableNearAlloc> Display for Arc<T, A> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.as_ref().fmt(f)
    }
}

impl<T: PartialEq, A: CloneableNearAlloc> PartialEq for Arc<T, A> {
    fn eq(&self, other: &Self) -> bool {
        self.as_ref().eq(other.as_ref())
    }
}

impl<T: Eq, A: CloneableNearAlloc> Eq for Arc<T, A> {}

impl<T: PartialOrd, A: CloneableNearAlloc> PartialOrd for Arc<T, A> {
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

impl<T: Ord, A: CloneableNearAlloc> Ord for Arc<T, A> {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.as_ref().cmp(other.as_ref())
    }
}

impl<T: Hash, A: CloneableNearAlloc> Hash for Arc<T, A> {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.as_ref().hash(state);
    }
}
