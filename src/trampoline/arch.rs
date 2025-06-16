//#[cfg(target_arch = "aarch64")]
pub mod aarch64;

//#[cfg(target_arch = "x86_64")]
pub mod x86_64;

//#[cfg(target_arch = "x86")]
pub mod x86;

//#[cfg(all(target_arch = "arm", thumb_mode))]
pub mod a32;

//#[cfg(all(target_arch = "arm", thumb_mode))]
pub mod t32;

pub mod traits {
    use core::{pin::Pin, sync::atomic::AtomicPtr};

    /// A sequence of instructions responsible for rerouting a function call to an
    /// arbitrary address.
    ///
    /// This is typically used when hooking requires that the hook procedure be within a
    /// certain address range of the function being hooked.
    ///
    /// # Safety
    /// After [`Self::init`](Thunk::init) is called the thunk's bytes must be valid
    /// machine code which, when executed, sets the instruction pointer
    /// to [`Self::target`](Thunk::target).
    ///
    /// Ideally, this should be the sole effect; no other registers or flags should be
    /// modified. On architectures where this is not possible, thunks should only
    /// clobber registers reserved for this by linker thunks (e.g. ip0/ip1 on aarch64).
    pub unsafe trait Thunk: Send + Sync {
        /// For thunks not implementing [`Unpin`], initialize any address sensitive
        /// state after pinning.
        fn init(self: Pin<&mut Self>) {}

        /// Get an atomic pointer to the target of this thunk.
        fn target(&self) -> &AtomicPtr<u8>;
    }
}
