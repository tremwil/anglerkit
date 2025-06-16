use core::alloc::Layout;
use core::ops::Range;
use core::ptr::NonNull;

use lock_api::{Mutex, RawMutex};
use talc::{Span, Talc};

use super::NearAllocator;
use crate::alloc::{AllocError, AsMutPtr};
use crate::liballoc::collections::BTreeMap;

// Just an estimation. We
#[cfg(target_pointer_width = "64")]
const TALC_STATE_SIZE: usize = 1024;

#[cfg(target_pointer_width = "32")]
const TALC_STATE_SIZE: usize = 512;

struct NearBlock<B: NearAllocator> {
    heap_span: Span,
    allocator: talc::Talc<talc::ErrOnOom>,
    alloc_count: u64,
    base: B::Ptr,
    layout: Layout,
}

// Sort the blocks by the end of their heap span.
type NearBlockTree<B> = BTreeMap<usize, NearBlock<B>>;

/// A [`NearAllocator`] that allocates big chunks from another possibly less performant
/// implementation (e.g. requesting virtual memory from the OS), and re-uses the memory
/// within these chunks effectively through the [`talc`] allocator.
pub struct BlockNearAlloc<R: RawMutex, B: NearAllocator> {
    blocks: Mutex<R, NearBlockTree<B>>,
    block_allocator: B,
    min_block: Layout, // note: Need at least 2KiB for talc
}

unsafe impl<R: RawMutex, B: NearAllocator + Send> Send for BlockNearAlloc<R, B> {}
unsafe impl<R: RawMutex, B: NearAllocator + Sync> Sync for BlockNearAlloc<R, B> {}

impl<R: RawMutex, B: NearAllocator> Drop for BlockNearAlloc<R, B> {
    fn drop(&mut self) {
        let mut moved_blocks = BTreeMap::default();
        core::mem::swap(&mut moved_blocks, self.blocks.get_mut());
        for (_, block) in moved_blocks {
            // SAFETY:
            // We allocated `block.base` using `block.allocator` and `block.layout`
            unsafe { self.block_allocator.free(block.base, block.layout) };
        }
    }
}

impl<R: RawMutex, B: NearAllocator> BlockNearAlloc<R, B> {
    /// Try to use an existing allocated block to allocate
    ///
    /// # Safety
    /// `layout.size()` must be nonzero.
    unsafe fn alloc_within_existing(
        blocks: &mut lock_api::MutexGuard<'_, R, NearBlockTree<B>>,
        addr_range: Range<usize>,
        layout: Layout,
    ) -> Result<NonNull<u8>, ()> {
        let span: Span = (addr_range.start as *mut u8..addr_range.end as _).into();

        // We include the end address, since they could match
        let inclusive_range = addr_range.start..=addr_range.end;
        for (_, block) in blocks.range_mut(inclusive_range) {
            if span.contains_span(block.heap_span) {
                // Safety: layout.size() != 0 is asserted by the caller
                match unsafe { block.allocator.malloc(layout) } {
                    Ok(ptr) => {
                        block.alloc_count += 1;
                        return Ok(ptr);
                    }
                    Err(_) => continue,
                }
            }
        }
        Err(())
    }

    /// Free all blocks that have zero active allocations.
    pub fn free_unused_blocks(&self) {
        let mut blocks = self.blocks.lock();

        // Without nightly, this is the best way to do it without requiring
        // <B as NearAllocator::Ptr>: Clone
        let mut moved_blocks = BTreeMap::default();
        core::mem::swap(&mut moved_blocks, &mut *blocks);
        for (end, block) in moved_blocks {
            if block.alloc_count == 0 {
                unsafe { self.block_allocator.free(block.base, block.layout) };
                continue;
            }
            blocks.insert(end, block);
        }
    }
}

unsafe impl<R: RawMutex, B: NearAllocator> NearAllocator for BlockNearAlloc<R, B> {
    type Ptr = NonNull<u8>;

    unsafe fn alloc_within(
        &self,
        range: Range<usize>,
        layout: Layout,
    ) -> Result<NonNull<u8>, AllocError> {
        let aligned_start = range
            .start
            .checked_next_multiple_of(layout.align())
            .ok_or(AllocError::NoSuitableRegion)?;

        if aligned_start.saturating_add(layout.size()) > range.end {
            return Err(AllocError::NoSuitableRegion);
        }

        let mut blocks = self.blocks.lock();
        match unsafe { Self::alloc_within_existing(&mut blocks, aligned_start..range.end, layout) }
        {
            Ok(ptr) => return Ok(ptr),
            Err(_) => (),
        };

        // Make sure we'll have space for talc's metadata and at least the requested layout
        let new_block_layout = Layout::from_size_align(
            self.min_block.size().max(layout.pad_to_align().size() + TALC_STATE_SIZE),
            self.min_block.align().max(layout.align()),
        )
        .map_err(|_| AllocError::UnsupportedLayout)?
        .pad_to_align();

        // Allocate from the underlying NearAllocator
        let base = unsafe { self.block_allocator.alloc_within(range, new_block_layout)? };

        // Initialize a talc instance for the block
        let alloc_span = Span::from_base_size(base.as_mut_ptr(), new_block_layout.size());
        let mut talc = Talc::new(talc::ErrOnOom);
        let heap_span = unsafe { talc.claim(alloc_span).unwrap() };

        // Allocate the requested layout before putting the new allocator in the blocks tree
        let ptr = unsafe { talc.malloc(layout) };

        blocks.insert(
            heap_span.get_base_acme().unwrap().1.addr(),
            NearBlock {
                heap_span,
                allocator: talc,
                alloc_count: ptr.map(|_| 1).unwrap_or(0),
                base,
                layout: new_block_layout,
            },
        );

        ptr.map_err(|_| AllocError::UnsupportedLayout)
    }

    unsafe fn free(&self, ptr: NonNull<u8>, layout: Layout) {
        let range = ptr.as_ptr().addr()..usize::MAX;
        let mut blocks = self.blocks.lock();
        let (_, block) = blocks.range_mut(range).next().unwrap();

        debug_assert!(block.alloc_count > 0);
        block.alloc_count -= 1;

        // SAFETY:
        // - user asserts that `ptr` was allocated with `layout`,
        // - we allocated ptr using `block.allocator`
        unsafe { block.allocator.free(ptr, layout) };
    }
}
