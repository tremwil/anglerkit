use core::sync::atomic::{AtomicPtr, Ordering};

/// x86_64 indirect jump thunk.
#[repr(C)]
pub struct Thunk {
    /// Rip-relative jump to target code address.
    ///
    /// jmp qword ptr [RIP+1 -> target] (66 FF 2D 01 00 00 00)
    _jmp_rip_mem: [u8; 7],
    /// Padding for `target` to be 8-byte aligned.
    _pad: u8,
    /// The code address to jump to.
    target: AtomicPtr<u8>,
}

impl Thunk {
    pub fn new(target: *const u8) -> Self {
        Self {
            _jmp_rip_mem: *b"\x66\xFF\x2D\x01\x00\x00\x00",
            _pad: 0xCC,
            target: AtomicPtr::new(target as *mut _),
        }
    }
}

unsafe impl super::traits::Thunk for Thunk {
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
