use core::{alloc::Layout, mem::ManuallyDrop, ops::Range};
use std::sync::Once;

use region::Allocation;

use super::NearAllocator;
use crate::alloc::{AllocError, AsMutPtr};

/// Stateless [`NearAllocator`] implemented through the [`region`] crate.
///
/// It is not recommended to use this allocator directly. Instead, wrap it inside a
/// [`BlockNearAlloc`](super::block::BlockNearAlloc) to reduce the amount of
/// virtual memory system calls required
#[derive(Debug, Clone, Copy, Default)]
pub struct RegionNearAlloc;

impl RegionNearAlloc {}

fn allocation_granularity() -> usize {
    static mut ALLOC_GRAN: usize = 0;
    static INIT: Once = Once::new();

    unsafe {
        INIT.call_once(|| {
            #[cfg(unix)]
            // should be true for all unixes supported by region-rs
            let alloc_gran = region::page::size();

            #[cfg(windows)]
            // TODO: use GetSystemInfo
            let alloc_gran = 1 << 16;

            assert!(alloc_gran.is_power_of_two());
            ALLOC_GRAN = alloc_gran;
        });
        ALLOC_GRAN
    }
}

pub struct Ptr {
    // the user's memory may not be aligned with the system's allocation granularity.
    user_base: *mut u8,
    alloc_handle: ManuallyDrop<Allocation>,
}

impl<T> AsMutPtr<T> for Ptr {
    fn as_mut_ptr(&self) -> *mut T {
        self.user_base.cast()
    }
}

fn try_alloc_in_gap(
    gap: Range<usize>,
    range: Range<usize>,
    layout: Layout,
) -> Option<Result<Ptr, AllocError>> {
    // We need to constain the span we can use to store the layout.
    //
    // edge case 1: can allocate near start of range
    // gap[   GRAN|    range[ <avail>  GRAN|  gap]        range]
    //
    // edge case 2: can allocate near end of range
    // range[    gap[   GRAN|      <avail>   range]     GRAN|  gap]
    //
    // To handle both, we calculate the allocatable area by aligning the gap to the
    // allocator granularity. Then, intersect with the target range.

    let alloc_gran = allocation_granularity();

    let min_boundary = gap.start.checked_next_multiple_of(alloc_gran)?;
    let max_boundary = gap.end & (alloc_gran - 1);

    let user_base = min_boundary.max(range.start).checked_next_multiple_of(layout.align())?;
    if user_base.checked_add(layout.size())? > max_boundary.min(range.end) {
        return None;
    }
    Some(
        region::alloc_at(
            user_base as *const u8,
            layout.size(),
            region::Protection::READ_WRITE_EXECUTE,
        )
        .map(|alloc| Ptr {
            user_base: user_base as *mut u8,
            alloc_handle: ManuallyDrop::new(alloc),
        })
        .map_err(|_| AllocError::OsError),
    )
}

unsafe impl NearAllocator for RegionNearAlloc {
    type Ptr = Ptr;

    unsafe fn alloc_within(
        &self,
        range: Range<usize>,
        layout: core::alloc::Layout,
    ) -> Result<Self::Ptr, super::AllocError> {
        let alloc_gran = allocation_granularity();

        let aligned_start = range
            .start
            .checked_next_multiple_of(layout.align())
            .ok_or(AllocError::NoSuitableRegion)?;

        if aligned_start.saturating_add(layout.size()) > range.end {
            return Err(AllocError::NoSuitableRegion);
        }

        let search_start = aligned_start & (alloc_gran - 1);
        let search_end = range.end.saturating_add(alloc_gran - 1) & (alloc_gran - 1);
        // note: size cannot overflow due to above range space check
        let mut region_iter =
            region::query_range(search_start as *const u8, search_end - search_start)
                .map_err(|_| AllocError::OsError)?;

        let mut last_region_end = search_start;
        while let Some(res) = region_iter.next() {
            match res {
                Ok(r) => {
                    match try_alloc_in_gap(
                        last_region_end..r.as_range().start,
                        range.clone(),
                        layout,
                    ) {
                        Some(Ok(ptr)) => return Ok(ptr),
                        // ignore the alloc error case here, we can keep trying
                        _ => last_region_end = r.as_range().end,
                    }
                }
                Err(_) => return Err(AllocError::OsError),
            }
        }

        return try_alloc_in_gap(last_region_end..search_end, range, layout)
            .ok_or(AllocError::NoSuitableRegion)
            .and_then(|res| res);
    }

    unsafe fn free(&self, mut ptr: Self::Ptr, _layout: core::alloc::Layout) {
        unsafe { ManuallyDrop::drop(&mut ptr.alloc_handle) };
    }
}
