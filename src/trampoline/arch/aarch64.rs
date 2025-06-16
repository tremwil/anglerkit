use core::{marker::PhantomPinned, pin::Pin, ptr::null, sync::atomic::AtomicPtr};

/// aarch64 indirect jump thunk.
#[repr(C)]
pub struct Thunk {
    /// Load self.target into ip0 (x16).
    _ldr_ip0: [u8; 4],
    /// Branch to ip0 (x16).
    _br_ip0: [u8; 4],
    target: AtomicPtr<u8>,
}

impl Thunk {
    pub fn new(target: *const u8) -> Self {
        Self {
            _ldr_ip0: *b"\x50\x00\x00\x58",
            _br_ip0: *b"\x00\x02\x1f\xd6",
            target: AtomicPtr::new(target as *mut _),
        }
    }
}

unsafe impl super::traits::Thunk for Thunk {
    fn target(&self) -> &AtomicPtr<u8> {
        &self.target
    }
}
