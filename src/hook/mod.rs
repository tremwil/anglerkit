use core::{
    mem::ManuallyDrop,
    sync::atomic::{AtomicBool, Ordering},
};

pub mod ptr;

use closure_ffi::{
    BareFnSync,
    traits::{FnPtr, FnThunk},
};
use liballoc::sync::Arc;

pub unsafe trait RawHook {
    fn original(&self) -> *const ();

    unsafe fn enable(&self) -> crate::Result<()>;

    unsafe fn disable(&self) -> crate::Result<()>;

    unsafe fn uninstall(self) -> crate::Result<()>;
}

pub struct LeakyHook<'a, R: RawHook, B: FnPtr> {
    raw: R,
    _bare: ManuallyDrop<BareFnSync<'a, B>>,
}

impl<'a, R: RawHook, B: FnPtr> LeakyHook<'a, R, B> {
    pub(crate) fn new(raw: R, bare: BareFnSync<'a, B>) -> Self {
        Self {
            raw,
            _bare: ManuallyDrop::new(bare),
        }
    }

    pub fn original(&self) -> B {
        unsafe { B::from_ptr(self.raw.original()) }
    }

    pub unsafe fn enable(&self) -> crate::Result<()> {
        unsafe { self.raw.enable() }
    }

    pub unsafe fn disable(&self) -> crate::Result<()> {
        unsafe { self.raw.disable() }
    }
}

impl<'a, R: RawHook, B: FnPtr> Drop for LeakyHook<'a, R, B> {
    fn drop(&mut self) {
        unsafe {
            let _ = self.raw.disable();
        }
    }
}

pub struct ToggleableHook<'a, R: RawHook, B: FnPtr> {
    raw: R,
    enabled: Arc<AtomicBool>,
    _bare: ManuallyDrop<BareFnSync<'a, B>>,
}

impl<'a, R: RawHook, B: FnPtr> ToggleableHook<'a, R, B> {
    pub fn original(&self) -> B {
        unsafe { B::from_ptr(self.raw.original()) }
    }

    pub fn enable(&self) {
        self.enabled.store(true, Ordering::Release);
    }

    pub fn disable(&self) {
        self.enabled.store(false, Ordering::Release);
    }

    pub unsafe fn enable_raw(this: &Self) -> crate::Result<()> {
        unsafe { this.raw.enable() }
    }

    pub unsafe fn disable_raw(this: &Self) -> crate::Result<()> {
        unsafe { this.raw.disable() }
    }
}

impl<'a, R: RawHook, B: FnPtr> Drop for ToggleableHook<'a, R, B> {
    fn drop(&mut self) {
        self.disable();
    }
}
