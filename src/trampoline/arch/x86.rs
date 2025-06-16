use core::{marker::PhantomPinned, pin::Pin, ptr::null, sync::atomic::AtomicPtr};

#[repr(C, packed)]
struct JmpMem {
    opcode: [u8; 2],
    address: *const AtomicPtr<u8>,
}

/// x86 indirect jump thunk.
#[repr(C)]
pub struct Thunk {
    /// Absolute jump to target code address.
    ///
    /// jmp qword ptr [.target] (FF 25 &self.target)
    jmp_mem: JmpMem,
    /// Padding for `target` to be 4-byte aligned.
    _pad: [u8; 2],
    /// The code address to jump to.
    target: AtomicPtr<u8>,
    _unpin: PhantomPinned,
}

impl Thunk {
    pub fn new(target: *const u8) -> Self {
        Self {
            jmp_mem: JmpMem {
                opcode: [0xFF, 0x25],
                address: null(),
            },
            _pad: [0xCC; 2],
            target: AtomicPtr::new(target as *mut _),
            _unpin: PhantomPinned,
        }
    }
}

unsafe impl Send for Thunk {}
unsafe impl Sync for Thunk {}

unsafe impl super::traits::Thunk for Thunk {
    fn init(self: Pin<&mut Self>) {
        // SAFETY:
        // - self is not moved
        // - using write_unaligned to write to the packed struct field
        unsafe {
            let this = self.get_unchecked_mut();
            (&raw mut this.jmp_mem.address).write_unaligned(&raw const this.target);
        };
    }

    fn target(&self) -> &AtomicPtr<u8> {
        &self.target
    }
}

pub struct Trampoline {
    /// Instructions displaced by the hook, padded with NOPs.
    /// The maximum size of an x86 instruction is 15 bytes.
    nop_padded_instructions: [u8; 15],
    /// 32-bit rip-relative jump instruction.
    _jmp_rel32_opcode: u8,
    jmp_target: i32,
}
