use core::marker::PhantomPinned;
use core::mem::{ManuallyDrop, MaybeUninit};
use core::ptr::null_mut;
use core::sync::atomic::{AtomicPtr, AtomicU64, AtomicUsize, Ordering::*};

use super::Error;

type AtomicIndex = AtomicU64;
type Index = u64;

#[cfg(not(target_has_atomic = "64"))]
compile_error!("Not supported on targets without hardware support for 64-bit atomic operations");

/// Number of index high bits used to store the generation
///
/// The more, the better the ABA problem mitigation.
const GEN_BITS: Index = 32;
const INDEX_MASK: Index = Index::MAX >> GEN_BITS;
const GEN_INCREMENT: Index = 1 << (Index::BITS as Index - GEN_BITS);

#[inline(always)]
fn untagged_index(tagged: Index) -> usize {
    (tagged & INDEX_MASK) as usize
}

/// Pointer to an allocated slot inside an [`AtomicSlab`].
pub struct AtomicSlabPtr<T> {
    ptr: *mut T,
    tagged_index: Index,
}

impl<T> Clone for AtomicSlabPtr<T> {
    fn clone(&self) -> Self {
        Self {
            ptr: self.ptr,
            tagged_index: self.tagged_index,
        }
    }
}

impl<T> Copy for AtomicSlabPtr<T> {}

impl<T> core::fmt::Debug for AtomicSlabPtr<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AtomicSlabPtr")
            .field("ptr", &self.ptr)
            .field("tagged_index", &self.tagged_index)
            .finish()
    }
}

impl<T> core::ops::Deref for AtomicSlabPtr<T> {
    type Target = *mut T;

    fn deref(&self) -> &Self::Target {
        &self.ptr
    }
}

impl<T> PartialEq for AtomicSlabPtr<T> {
    fn eq(&self, other: &Self) -> bool {
        let result = self.ptr() == other.ptr();
        // If two pointers share the same slot, we should consider them the same
        // from a user perspective, even if their generation count is different.
        //
        // However, comparing such pointers is a strong hint that there is a bug in the
        // user's code, as it implies that they are still using a freed pointer.
        debug_assert_eq!(
            self.tagged_index, other.tagged_index,
            "distinct generation (strong possibility of a use-after free)"
        );

        result
    }
}

impl<T> Eq for AtomicSlabPtr<T> {}

impl<T> PartialOrd for AtomicSlabPtr<T> {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        self.ptr().partial_cmp(&other.ptr())
    }
}

impl<T> Ord for AtomicSlabPtr<T> {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.ptr().cmp(&other.ptr())
    }
}

impl<T> core::hash::Hash for AtomicSlabPtr<T> {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.ptr().hash(state)
    }
}

impl<T> AtomicSlabPtr<T> {
    /// # Safety
    /// - `untagged_index(tagged_index)` must be less than the length of the buffer
    ///   pointed at by `storage_ptr`.
    #[inline(always)]
    unsafe fn new(storage_ptr: *mut AtomicSlabSlot<T>, tagged_index: Index) -> Self {
        Self {
            ptr: storage_ptr.add(untagged_index(tagged_index)).cast(),
            tagged_index,
        }
    }

    #[inline(always)]
    pub fn ptr(&self) -> *mut T {
        self.ptr
    }
}

union AtomicSlabSlot<T> {
    free_next: ManuallyDrop<AtomicIndex>,
    _value: ManuallyDrop<MaybeUninit<T>>,
}

#[repr(align(128))]
#[derive(Default, Clone, Copy)]
struct CachePadded<T>(pub T);

impl<T> core::ops::Deref for CachePadded<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl<T> core::ops::DerefMut for CachePadded<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

/// Thread-safe, lock-free slab allocator for a single type over pre-allocated
/// storage.
///
/// Extremely cheap O(1) allocation and deallocation with constant memory
/// overhead when `T` is larger or equal to a pointer.
///
/// As it does not use sharding or other forms of thread-local state,
/// performance will be severely affected by high contention.
///
/// # Note
///
/// This data structure uses tagged indices in its free list to encode a
/// per-slot "generation" (deallocation count). This mitigates, *but not
/// entirely prevent*, the ABA problem. Hence there is still possibility of data
/// races, although it is astronomically tiny.
#[repr(C)]
pub struct AtomicSlab<T> {
    free_head: CachePadded<AtomicIndex>,
    storage: *mut AtomicSlabSlot<T>,
    capacity: usize,
    len: CachePadded<AtomicUsize>,
}

unsafe impl<T> Send for AtomicSlab<T> {}
unsafe impl<T> Sync for AtomicSlab<T> {}

impl<T> AtomicSlab<T> {
    /// Maximum size of a slab, in elements.
    ///
    /// Internally, is also used as a sentinel value for a "null" index.
    pub const MAX_CAPACITY: usize = INDEX_MASK as usize;

    /// Maximum capacity of the slab storage, in bytes.
    ///
    /// If a larger buffer is passed to [`AtomicSlab::new`], the extra bytes
    /// will be left unused.
    pub const MAX_CAPACITY_BYTES: usize = Self::MAX_CAPACITY * size_of::<AtomicSlabSlot<T>>();

    /// Create a new [`AtomicSlab`] given pre-allocated storage.
    ///
    /// # Safety
    /// - no accesses through the `storage` pointer can be made after passing it to this
    /// function until the slab is dropped.
    pub unsafe fn new(raw_storage: *mut [u8]) -> Self {
        let layout = core::alloc::Layout::new::<AtomicSlabSlot<T>>();
        let align_offset = (raw_storage as *mut u8).align_offset(layout.align());
        let storage = raw_storage.map_addr(|a| a + align_offset).cast();
        let capacity = raw_storage.len().saturating_sub(align_offset) / layout.size();

        Self {
            free_head: CachePadded(AtomicIndex::new(Self::MAX_CAPACITY as Index)),
            storage,
            capacity: (capacity as usize).min(Self::MAX_CAPACITY),
            len: CachePadded(AtomicUsize::new(0)),
        }
    }

    /// Get the minimum address at which a value of `T` may be located in this
    /// slab.
    pub fn min_address(&self) -> usize {
        self.storage.addr()
    }

    /// Get the maximum address at which a value of `T` may be located in this
    /// slab.
    pub fn max_address(&self) -> usize {
        unsafe { self.storage.add(self.capacity.saturating_sub(1)).addr() }
    }

    /// Try to allocate memory for a value of type `T`.
    ///
    /// The provided memory is **uninitialized**.
    ///
    /// Fails with [`Error::OutOfMemory`] if the slab's storage buffer is full.
    pub fn alloc(&self) -> Result<AtomicSlabPtr<T>, Error> {
        // Need to:
        // 1. Check freelist head
        // 2. If freelist head is NULL, bump used_len to reserve the next avail. slot
        // 3. If free slot exists, claim it and set freelist head to next slot

        let mut head = self.free_head.load(Acquire);
        loop {
            let head_index = untagged_index(head);
            // No slot in free list, grab an uninitialized slot
            if head_index == Self::MAX_CAPACITY {
                let slot = self.len.fetch_add(1, Relaxed);
                // Instead of using a CAS, just reset it. We don't care how accurate len
                // gets once the storage is full
                if slot >= self.capacity {
                    self.len.store(self.capacity, Relaxed);
                    return Err(Error::OutOfMemory);
                }
                // SAFETY:
                // - slot is within storage buffer and atomically incremented,
                // - slot is less then capacity
                return Ok(unsafe { AtomicSlabPtr::new(self.storage, slot as Index) });
            }

            debug_assert!(
                head_index < self.capacity,
                "{head_index} >= capacity ({}), logic bug or data race",
                self.capacity
            );

            // SAFETY:
            // - untagged head index is always less than `capacity`
            // - Slot is in the freelist, so access to `free_next` is not aliasing
            //
            // This can race with someone writing to the just-allocated allocated `head`.
            // However, this is only a problem when the CAS succeeds, in which case:
            // 1. `head` was still `self.free_head`.
            // 2. `head` was the last freed slot, and has not been alloc'd since, due to generation
            //   tagging.
            // 3. the last `head` CAS store in free() has release ordering, so *synchronizes with*
            //   the current head load (acquire).
            // 4. the relaxed store to `free_next` in free() *happens-before* the successful CAS.
            // 5. the caller of `free(ptr)` asserts that all read/writes to `ptr`, which is sharing
            //   storage with `free_next`, *happen-before* the call.
            // 6. so, by (3) and transitivity, the current thread should observe the following
            //    ordering for `free_next`: user read/writes -> free() `free_next` store.
            // 7. hence the load below will read the correct `free_next` value.
            let next = unsafe { (*self.storage.add(head_index)).free_next.load(Relaxed) };
            match self.free_head.compare_exchange_weak(head, next, Release, Acquire) {
                // SAFETY: head index is valid and < capacity (see above)
                Ok(_) => return Ok(unsafe { AtomicSlabPtr::new(self.storage, head) }),
                Err(new_head) => head = new_head,
            }
        }
    }

    /// Free an allocated pointer to a value of `T`.
    ///
    /// # Safety
    /// - `ptr` must have been obtained from [`AtomicSlab::alloc`].
    /// - There can be no use-after-free. Specifically, all past reads and writes to
    ///   `ptr`'s target must synchronize (i.e. have an "happens-before" relationship)
    ///   with this method call.
    pub unsafe fn free(&self, ptr: AtomicSlabPtr<T>) {
        let index = untagged_index(ptr.tagged_index);
        debug_assert!(
            index < self.capacity,
            "{index} >= capacity ({}), logic bug or data race",
            self.capacity
        );

        // SAFETY: ptr has been obtained from `alloc`, so `tagged_index`'s untagged
        // index is within the storage buffer
        let slot = unsafe { self.storage.add(index) };
        let next_gen = ptr.tagged_index.wrapping_add(GEN_INCREMENT);

        // CAS to replace free_head with the next generation of ptr's index
        let _ = self.free_head.fetch_update(Release, Acquire, |head| {
            // SAFETY: This slot has been freed so it is empty, and we are the unique writer.
            // A thread allocating could be reading
            unsafe { (*slot).free_next.store(head, Relaxed) };
            Some(next_gen)
        });
    }
}

#[cfg(all(test, not(feature = "no_std")))]
mod tests {
    use std::{
        alloc::{alloc, dealloc, Layout},
        ptr::slice_from_raw_parts_mut,
        thread,
    };

    use rand::{Rng, RngCore};

    use super::{AtomicSlab, Relaxed};

    #[cfg(miri)]
    const NUM_OPS: usize = 1000;
    #[cfg(not(miri))]
    const NUM_OPS: usize = 10000000;

    #[test]
    fn test_ctor() {
        let layout = Layout::new::<[u128; 2048]>();
        let raw_storage = slice_from_raw_parts_mut(unsafe { alloc(layout) }, layout.size());

        let atomic_slab: AtomicSlab<u128> = unsafe { AtomicSlab::new(raw_storage) };
        assert!(atomic_slab.capacity == 2048);

        unsafe { dealloc(raw_storage.cast(), layout) };
    }

    #[test]
    fn test_fill_and_free() {
        let layout = Layout::new::<[u128; 2048]>();
        let raw_storage = slice_from_raw_parts_mut(unsafe { alloc(layout) }, layout.size());

        let atomic_slab: AtomicSlab<u128> = unsafe { AtomicSlab::new(raw_storage) };

        let mut rng = rand::rng();
        let mut pointers = Vec::new();
        for _ in 0..2048 {
            let ptr = atomic_slab.alloc().unwrap();
            let val = rng.random();
            unsafe { ptr.write(val) };
            pointers.push((ptr, val));
        }

        // Should have run out of memory here
        assert!(atomic_slab.alloc().is_err());

        // Check that all pointers still match the written value as we deallocate in a
        // random order
        while !pointers.is_empty() {
            let to_pop = rng.random_range(..pointers.len());
            let (ptr, value) = pointers.swap_remove(to_pop);
            assert_eq!(unsafe { ptr.read() }, value);
            unsafe { atomic_slab.free(ptr) };
        }

        // Reclaim the storage so miri doesn't complain
        unsafe {
            dealloc(raw_storage.cast(), layout);
        }
    }

    #[test]
    fn test_random_ops() {
        let layout = Layout::new::<[usize; 2048]>();
        let raw_storage = slice_from_raw_parts_mut(unsafe { alloc(layout) }, layout.size());

        let atomic_slab: AtomicSlab<usize> = unsafe { AtomicSlab::new(raw_storage) };

        let mut rng = rand::rng();
        let mut pointers = Vec::new();

        let mut max_used_slots = 0;
        for _ in 0..NUM_OPS {
            if rng.random() {
                if let Ok(ptr) = atomic_slab.alloc() {
                    let value = rng.next_u64() as usize;
                    unsafe { ptr.write(value) };
                    pointers.push((ptr, value));
                    max_used_slots = max_used_slots.max(pointers.len());
                }
            }
            else if !pointers.is_empty() {
                let to_pop = rng.random_range(..pointers.len());
                let (ptr, value) = pointers.swap_remove(to_pop);

                assert_eq!(unsafe { ptr.read() }, value);
                unsafe { atomic_slab.free(ptr) }
            }
        }

        // Ensure we only grew by the optimal amount
        assert_eq!(max_used_slots, atomic_slab.len.0.load(Relaxed));

        // Reclaim the storage so miri doesn't complain
        unsafe {
            dealloc(raw_storage.cast(), layout);
        }
    }

    #[test]
    #[cfg(not(miri))]
    fn test_concurrent_ops() {
        const CAPACITY: usize = 1 << 16;

        let layout = Layout::new::<[usize; CAPACITY]>();
        let raw_storage = slice_from_raw_parts_mut(unsafe { alloc(layout) }, layout.size());

        let atomic_slab = unsafe { AtomicSlab::<usize>::new(raw_storage) };

        thread::scope(|s| {
            for _ in 0..4 {
                s.spawn(|| {
                    let mut rng = rand::rng();
                    let mut pointers = Vec::new();

                    for _ in 0..NUM_OPS {
                        if rng.random() {
                            if let Ok(ptr) = atomic_slab.alloc() {
                                let value = rng.next_u64() as usize;
                                unsafe { ptr.ptr().write(value) };
                                pointers.push((ptr, value));
                            }
                        }
                        else if !pointers.is_empty() {
                            let to_pop = rng.random_range(..pointers.len());
                            let (ptr, value) = pointers.swap_remove(to_pop);

                            assert_eq!(unsafe { ptr.ptr().read() }, value);
                            unsafe { atomic_slab.free(ptr) }
                        }
                    }
                });
            }
        });

        // Reclaim the storage so miri doesn't complain
        unsafe {
            dealloc(raw_storage.cast(), layout);
        }
    }
}
