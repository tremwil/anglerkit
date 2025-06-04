use core::marker::PhantomPinned;
use core::pin::Pin;
#[cfg(all(not(loom), not(feature = "no_std")))]
use std::sync::RwLock;

#[cfg(loom)]
use loom::sync::RwLock;
#[cfg(all(not(loom), feature = "no_std"))]
use spin::RwLock;

use super::atomic_slab::{AtomicSlab, AtomicSlabPtr};
use super::{AsPtr, NearAllocator, ThunkAllocator};
use crate::liballoc::boxed::Box;
use crate::liballoc::collections::BTreeMap;

struct PinnedAtomicSlab<T> {
    slab: AtomicSlab<T>,
    base_address: *mut u8,
    _phantom: PhantomPinned,
}

impl<T> PinnedAtomicSlab<T> {
    /// SAFETY: Same requirements as [`AtomicSlab::new`]
    unsafe fn new(buffer: *mut [u8]) -> Pin<Box<Self>> {
        Box::pin(Self {
            slab: AtomicSlab::new(buffer),
            base_address: buffer as *mut u8,
            _phantom: PhantomPinned,
        })
    }
}

type SlabTree<T> = BTreeMap<usize, Pin<Box<PinnedAtomicSlab<T>>>>;

/// A [`ThunkAllocator`] implementation which uses a pool of lock-free slab
/// allocators over fixed memory blocks.
///
/// The blocks are allocated using the provided [`NearAllocator`].
pub struct SlabPool<T, A: NearAllocator> {
    near_alloc: A,
    slab_size: usize,
    slabs: RwLock<SlabTree<T>>,
}

impl<T, A: NearAllocator> Drop for SlabPool<T, A> {
    fn drop(&mut self) {
        // Free the slab storage
        for (_, slab) in handle_poison(self.slabs.get_mut()).iter() {
            // SAFETY: `base_address` was allocated with `near_alloc`
            unsafe { self.near_alloc.free(slab.base_address) };
        }
    }
}

/// Opaque pointer to a [`SlabPool`] allocation. Returned by
pub struct SlabPoolPtr<T> {
    slab: *const AtomicSlab<T>,
    ptr: AtomicSlabPtr<T>,
}

impl<T> AsPtr<T> for SlabPoolPtr<T> {
    fn as_ptr(&self) -> *const T {
        self.ptr.ptr().cast()
    }

    fn as_ptr_mut(&mut self) -> *mut T {
        self.ptr.ptr().cast()
    }
}

impl<T, A: NearAllocator> SlabPool<T, A> {
    pub fn new(near_alloc: A, slab_size: usize) -> Self {
        Self {
            near_alloc,
            slab_size: slab_size.min(AtomicSlab::<T>::MAX_CAPACITY_BYTES),
            slabs: Default::default(),
        }
    }

    fn alloc_with_existing(
        slabs: &impl core::ops::Deref<Target = SlabTree<T>>,
        addr_range: core::ops::Range<usize>,
    ) -> Option<Result<SlabPoolPtr<T>, super::Error>> {
        for (_, slab) in slabs.range(addr_range.clone()) {
            let slab = &slab.slab;
            if slab.max_address() < addr_range.end {
                match slab.alloc() {
                    Ok(ptr) => return Some(Ok(SlabPoolPtr { slab, ptr })),
                    Err(super::Error::OutOfMemory) => continue,
                    Err(other) => return Some(Err(other)),
                }
            }
        }
        None
    }
}

#[cfg(all(not(loom), feature = "no_std"))]
fn handle_poison<T>(maybe_guard: T) -> T {
    maybe_guard
}

#[cfg(any(loom, not(feature = "no_std")))]
fn handle_poison<T, R>(maybe_guard: Result<T, R>) -> T {
    maybe_guard.map_err(|_| ()).expect("lock poisoned")
}

unsafe impl<T, A: NearAllocator> ThunkAllocator<T> for SlabPool<T, A> {
    type Ptr = SlabPoolPtr<T>;

    fn alloc_near(&self, address: usize, displacement: usize) -> Result<Self::Ptr, super::Error> {
        // Try to find a slab in range
        let min_addr = address.saturating_sub(displacement);
        let max_addr = address.saturating_add(displacement).saturating_sub(size_of::<T>());

        let slabs = handle_poison(self.slabs.read());
        if let Some(r) = Self::alloc_with_existing(&slabs, min_addr..max_addr) {
            return r;
        }

        // Acquire a write lock this time, and scan again
        let mut slabs = handle_poison(self.slabs.write());
        if let Some(r) = Self::alloc_with_existing(&slabs, min_addr..max_addr) {
            return r;
        }

        // If no suitable slabs are found, allocate another one using `near_alloc`
        let buffer = self.near_alloc.alloc_near(address, displacement, self.slab_size)?;

        // SAFETY: A is a NearAllocator
        let pinned_slab = unsafe { PinnedAtomicSlab::new(buffer) };
        let ptr = pinned_slab.slab.alloc();
        let slab = &raw const pinned_slab.slab;

        // Insert new slab into pools
        slabs.insert(pinned_slab.slab.min_address(), pinned_slab);

        ptr.map(|ptr| SlabPoolPtr { slab, ptr })
    }

    unsafe fn free(&self, ptr: Self::Ptr) {
        // SAFETY:
        // - The user asserts that `ptr` came from `alloc_near`
        // - AtomicSlab invariants are upheld
        unsafe { (&*(ptr.slab)).free(ptr.ptr) };
    }
}
