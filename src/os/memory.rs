//! #[no_std] compatible shims for some OS virtual memory management functions required by anglerkit.

use liballoc::{collections::btree_map::BTreeMap, vec::Vec};

use super::{OsErr, OsImpl};

// Code taken from the `region` crate
bitflags::bitflags! {
  #[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
  pub struct Protection: usize {
    /// No access allowed at all.
    const NONE = 0;
    /// Read access; writing and/or executing data will panic.
    const READ = (1 << 0);
    /// Write access; this flag alone may not be supported on all OSs.
    const WRITE = (1 << 1);
    /// Execute access; this may not be allowed depending on DEP.
    const EXECUTE = (1 << 2);
    /// Read and execute shorthand.
    const READ_EXECUTE = (Self::READ.bits() | Self::EXECUTE.bits());
    /// Read and write shorthand.
    const READ_WRITE = (Self::READ.bits() | Self::WRITE.bits());
    /// Read, write and execute shorthand.
    const READ_WRITE_EXECUTE = (Self::READ.bits() | Self::WRITE.bits() | Self::EXECUTE.bits());
    /// Write and execute shorthand.
    const WRITE_EXECUTE = (Self::WRITE.bits() | Self::EXECUTE.bits());
  }
}

pub struct MemoryRegion {
    pub base: *const (),
    pub size: usize,
    pub prot: Protection,
}

pub struct MemProtectGuard {
    regions: Vec<MemoryRegion>,
}

impl MemProtectGuard {
    pub unsafe fn new(regions: Vec<MemoryRegion>) -> Self {
        Self { regions }
    }

    pub fn regions(&self) -> &[MemoryRegion] {
        &self.regions
    }

    unsafe fn revert_internal(&mut self) -> Result<(), OsErr> {
        self.regions
            .iter()
            .try_for_each(|r| unsafe { OsImpl.mem_protect(r.base, r.size, r.prot) })
    }

    pub unsafe fn revert(self) -> Result<(), OsErr> {
        let mut no_drop = core::mem::ManuallyDrop::new(self);
        unsafe { no_drop.revert_internal() }
    }
}

impl Drop for MemProtectGuard {
    fn drop(&mut self) {
        let result = unsafe { self.revert_internal() };
        debug_assert!(
            result.is_ok(),
            "failed to revert protection of memory regions: {result:?}",
        )
    }
}

/// Dyn-compatible trait used to provide an implementation of OS virtual memory
/// management functions required by the crate.
///
/// # Safety
/// [`MemFunctions::page_size`] must be a power of two.
pub unsafe trait MemFunctions {
    /// Get the size of a memory page on the system (usually 4096 bytes).
    fn page_size(&self) -> usize;

    /// Queries committed regions of memory pages with contiguous protection flags that
    /// intersect the given range.
    fn mem_query(&self, addr: *const (), size: usize) -> Result<Vec<MemoryRegion>, OsErr>;

    /// Changes the protection flags of a contiguous region of committed memory pages
    /// that intersect the given range.
    unsafe fn mem_protect(
        &self,
        addr: *const (),
        size: usize,
        prot: Protection,
    ) -> Result<(), OsErr>;

    /// Changes the protection flags of a contiguous region of committed memory pages
    /// that intersect the given range, returning a [`MemProtectGuard`] struct to help
    /// restoring the memory protection of the range later.
    ///
    /// The default implementation of this method uses
    /// [`mem_query`](MemFunctions::mem_query) to fetch the original regions. It can be
    /// overridden in case this operation can be performed more efficiently on the
    /// target platform. For example, on Windows `VirtualProtect` returns the old
    /// protection of the first page, so if only one page is modified no
    /// [`mem_query`](MemFunctions::mem_query) call is required.
    unsafe fn mem_protect_guard(
        &self,
        addr: *const (),
        size: usize,
        prot: Protection,
    ) -> Result<MemProtectGuard, OsErr> {
        let page_end = (addr.addr() + size)
            .checked_next_multiple_of(self.page_size())
            .unwrap_or(self.page_size().wrapping_neg());
        let mut regions = self.mem_query(addr, size)?;

        // trim the size of the last region
        if let Some(r) = regions.last_mut()
            && r.base.addr() < page_end
        {
            r.size = page_end - r.base.addr();
        }

        unsafe { self.mem_protect(addr, size, prot)? }
        Ok(unsafe { MemProtectGuard::new(regions) })
    }
}

#[cfg(feature = "std")]
mod std_impl {
    use std::string::ToString;

    use region;

    use super::*;

    impl From<region::Error> for OsErr {
        fn from(value: region::Error) -> Self {
            Self::from_string(value.to_string())
        }
    }

    impl From<region::Protection> for Protection {
        fn from(value: region::Protection) -> Self {
            match value {
                region::Protection::NONE => Protection::NONE,
                region::Protection::READ => Protection::READ,
                region::Protection::WRITE => Protection::WRITE,
                region::Protection::EXECUTE => Protection::EXECUTE,
                region::Protection::READ_EXECUTE => Protection::READ_EXECUTE,
                region::Protection::READ_WRITE => Protection::READ_WRITE,
                region::Protection::READ_WRITE_EXECUTE => Protection::READ_WRITE_EXECUTE,
                region::Protection::WRITE_EXECUTE => Protection::WRITE_EXECUTE,
                _ => unreachable!("{value}"),
            }
        }
    }

    impl From<Protection> for region::Protection {
        fn from(value: Protection) -> Self {
            match value {
                Protection::NONE => region::Protection::NONE,
                Protection::READ => region::Protection::READ,
                Protection::WRITE => region::Protection::WRITE,
                Protection::EXECUTE => region::Protection::EXECUTE,
                Protection::READ_EXECUTE => region::Protection::READ_EXECUTE,
                Protection::READ_WRITE => region::Protection::READ_WRITE,
                Protection::READ_WRITE_EXECUTE => region::Protection::READ_WRITE_EXECUTE,
                Protection::WRITE_EXECUTE => region::Protection::WRITE_EXECUTE,
                _ => unreachable!("{value}"),
            }
        }
    }

    unsafe impl MemFunctions for OsImpl {
        fn page_size(&self) -> usize {
            region::page::size()
        }

        fn mem_query(&self, addr: *const (), size: usize) -> Result<Vec<MemoryRegion>, OsErr> {
            region::query_range(addr, size)?
                .map(|r| {
                    r.map(|r| MemoryRegion {
                        base: r.as_ptr(),
                        size: r.len(),
                        prot: r.protection().into(),
                    })
                    .map_err(Into::into)
                })
                .collect()
        }

        unsafe fn mem_protect(
            &self,
            addr: *const (),
            size: usize,
            prot: Protection,
        ) -> Result<(), OsErr> {
            unsafe { region::protect(addr, size, prot.into())? };
            Ok(())
        }

        #[cfg(target_os = "windows")]
        unsafe fn mem_protect_guard(
            &self,
            addr: *const (),
            size: usize,
            prot: Protection,
        ) -> Result<MemProtectGuard, OsErr> {
            use windows_sys::Win32::System::Memory::{
                PAGE_EXECUTE, PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE, PAGE_EXECUTE_WRITECOPY,
                PAGE_NOACCESS, PAGE_READONLY, PAGE_READWRITE, PAGE_WRITECOPY, VirtualProtect,
            };

            fn to_native_prot(prot: Protection) -> u32 {
                match prot {
                    Protection::NONE => PAGE_NOACCESS,
                    Protection::READ => PAGE_READONLY,
                    Protection::WRITE => PAGE_READWRITE,
                    Protection::EXECUTE => PAGE_EXECUTE,
                    Protection::READ_EXECUTE => PAGE_EXECUTE_READ,
                    Protection::READ_WRITE => PAGE_READWRITE,
                    Protection::READ_WRITE_EXECUTE => PAGE_EXECUTE_READWRITE,
                    Protection::WRITE_EXECUTE => PAGE_EXECUTE_READWRITE,
                    _ => unreachable!(),
                }
            }

            fn from_native_prot(prot: u32) -> Protection {
                // keep flags up to PAGE_EXECUTE_WRITECOPY
                match prot & 0xFF {
                    PAGE_EXECUTE => Protection::EXECUTE,
                    PAGE_EXECUTE_READ => Protection::READ_EXECUTE,
                    PAGE_EXECUTE_READWRITE => Protection::READ_WRITE_EXECUTE,
                    PAGE_EXECUTE_WRITECOPY => Protection::READ_WRITE_EXECUTE,
                    PAGE_NOACCESS => Protection::NONE,
                    PAGE_READONLY => Protection::READ,
                    PAGE_READWRITE => Protection::READ_WRITE,
                    PAGE_WRITECOPY => Protection::READ_WRITE,
                    _ => unreachable!("Protection: 0x{:X}", prot),
                }
            }

            let start_page = region::page::floor(addr);
            let end_page = region::page::ceil(unsafe { addr.byte_add(size) }).addr();

            // fast path that doesn't require mem_query
            if start_page.addr() == end_page {
                let win_prot = to_native_prot(prot);
                let mut old_protect = 0;
                let success =
                    unsafe { VirtualProtect(addr.cast(), size, win_prot, &mut old_protect) };

                return (success != 0)
                    .then(|| unsafe {
                        MemProtectGuard::new(std::vec![MemoryRegion {
                            base: start_page,
                            size: self.page_size(),
                            prot: from_native_prot(old_protect),
                        }])
                    })
                    .ok_or(OsErr::from_string(
                        std::io::Error::last_os_error().to_string(),
                    ));
            }

            let mut regions = self.mem_query(addr, size)?;

            // trim the size of the last region
            if let Some(r) = regions.last_mut()
                && r.base.addr() < end_page
            {
                r.size = end_page - r.base.addr();
            }
            unsafe { self.mem_protect(addr, size, prot)? }
            Ok(unsafe { MemProtectGuard::new(regions) })
        }
    }
}

#[cfg(not(feature = "std"))]
unsafe impl MemFunctions for OsImpl {
    fn page_size(&self) -> usize {
        super::no_std::os_impl().page_size()
    }

    fn mem_query(&self, addr: *const (), size: usize) -> Result<Vec<MemoryRegion>, OsErr> {
        super::no_std::os_impl().mem_query(addr, size)
    }

    unsafe fn mem_protect(
        &self,
        addr: *const (),
        size: usize,
        prot: Protection,
    ) -> Result<(), OsErr> {
        unsafe { super::no_std::os_impl().mem_protect(addr, size, prot) }
    }

    unsafe fn mem_protect_guard(
        &self,
        addr: *const (),
        size: usize,
        prot: Protection,
    ) -> Result<MemProtectGuard, OsErr> {
        unsafe { super::no_std::os_impl().mem_protect_guard(addr, size, prot) }
    }
}

struct GlobalGuardRegion {
    addr: *const (),
    size: usize,
    read: usize,
    write: usize,
    execute: usize,
}

struct GlobalMemProtectGuard {
    /// disjoint intervals ordered by base address on which we have a lower bound for
    /// the memory protection flags.
    known_regions: BTreeMap<*const (), GlobalGuardRegion>,
}
