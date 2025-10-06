use core::{
    marker::PhantomData,
    sync::atomic::{AtomicPtr, Ordering},
};

use closure_ffi::{
    BareFnSync, cc,
    traits::{FnPtr, FnThunk},
};
use liballoc::sync::Arc;

use crate::{
    hook::{LeakyHook, RawHook},
    os::{
        OsImpl,
        memory::{MemFunctions, Protection},
    },
};

pub struct RawPtrHook {
    target: *mut *const (),
    original: *const (),
    hook: *const (),
}

impl RawPtrHook {
    unsafe fn swap_target(&self, new: *const ()) -> crate::Result<*const ()> {
        unsafe {
            let guard = OsImpl.mem_protect_guard(
                self.target.cast(),
                size_of::<*const ()>(),
                Protection::READ_WRITE,
            )?;

            let target = AtomicPtr::from_ptr(self.target.cast());
            let old = target.swap(new.cast_mut(), Ordering::Relaxed);
            guard.revert()?; // explicit revert to forward the error to the caller
            Ok(old.cast_const())
        }
    }
}

unsafe impl RawHook for RawPtrHook {
    fn original(&self) -> *const () {
        self.original
    }

    unsafe fn enable(&self) -> crate::Result<()> {
        unsafe { self.swap_target(self.hook)? };
        Ok(())
    }

    unsafe fn disable(&self) -> crate::Result<()> {
        unsafe { self.swap_target(self.original)? };
        Ok(())
    }

    unsafe fn uninstall(self) -> crate::Result<()> {
        unsafe { self.disable() }
    }
}

pub struct Context<B: FnPtr> {
    original: Arc<spin::Once<B>>,
}

#[cfg(feature = "nightly")]
pub mod nightly {
    use core::marker::PhantomData;

    use closure_ffi::traits::FnPtr;

    pub struct CallableFnPtr<'a, 'b, 'c, B: FnPtr>(B, PhantomData<(&'a (), &'b (), &'c ())>);

    impl<'a, 'b, 'c, B: FnPtr> CallableFnPtr<'a, 'b, 'c, B> {
        pub(super) fn new(ptr: B) -> Self {
            Self(ptr, PhantomData)
        }
    }

    impl<'a, 'b, 'c, B: FnPtr> FnOnce<B::Args<'a, 'b, 'c>> for CallableFnPtr<'a, 'b, 'c, B> {
        type Output = B::Ret<'a, 'b, 'c>;
        extern "rust-call" fn call_once(self, args: B::Args<'a, 'b, 'c>) -> Self::Output {
            unsafe { self.0.call(args) }
        }
    }

    impl<'a, 'b, 'c, B: FnPtr> FnMut<B::Args<'a, 'b, 'c>> for CallableFnPtr<'a, 'b, 'c, B> {
        extern "rust-call" fn call_mut(&mut self, args: B::Args<'a, 'b, 'c>) -> Self::Output {
            unsafe { self.0.call(args) }
        }
    }

    impl<'a, 'b, 'c, B: FnPtr> Fn<B::Args<'a, 'b, 'c>> for CallableFnPtr<'a, 'b, 'c, B> {
        extern "rust-call" fn call(&self, args: B::Args<'a, 'b, 'c>) -> Self::Output {
            unsafe { self.0.call(args) }
        }
    }
}

impl<B: FnPtr> Context<B> {
    pub fn original_ptr(&self) -> B {
        *self.original.wait()
    }

    pub unsafe fn call_original<'a, 'b, 'c>(
        &self,
        args: B::Args<'a, 'b, 'c>,
    ) -> B::Ret<'a, 'b, 'c> {
        unsafe { self.original_ptr().call(args) }
    }

    #[cfg(feature = "nightly")]
    pub unsafe fn original(&self) -> nightly::CallableFnPtr<'static, 'static, 'static, B> {
        nightly::CallableFnPtr::new(self.original_ptr())
    }
}

pub mod builder {
    use core::{
        marker::PhantomData,
        mem::ManuallyDrop,
        sync::atomic::{AtomicBool, Ordering},
    };

    use closure_ffi::{
        BareFnSync, cc, thunk_factory,
        traits::{FnPtr, FnThunk},
    };
    use liballoc::sync::Arc;

    use crate::hook::{
        LeakyHook, ToggleableHook,
        ptr::{Context, RawPtrHook},
    };

    pub struct Builder<S = InitState> {
        state: S,
    }

    #[doc(hidden)]
    pub struct InitState;

    #[doc(hidden)]
    pub struct SigState<B: FnPtr> {
        target: *mut B,
    }

    #[doc(hidden)]
    pub struct CCState<CC, const HAS_TARGET: bool, const HAS_CC: bool> {
        target: *mut *const (),
        cc: CC,
    }

    #[doc(hidden)]
    pub struct ThunkState<'a, B: FnPtr, T: FnThunk<B> + Send + Sync + 'a> {
        target: *mut *const (),
        thunk: T,
        ctx_orig_setter: Option<Arc<spin::Once<B>>>,
        phantom: PhantomData<&'a ()>,
    }

    impl Default for Builder<InitState> {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Builder<InitState> {
        pub const fn new() -> Self {
            Self { state: InitState }
        }

        pub fn target_fn<B: FnPtr>(self, target: *mut B) -> Builder<SigState<B>> {
            Builder {
                state: SigState { target },
            }
        }

        pub fn target_ptr(self, target: *mut *const ()) -> Builder<CCState<cc::C, true, false>> {
            Builder {
                state: CCState { target, cc: cc::C },
            }
        }

        pub fn target_addr(self, target: usize) -> Builder<CCState<cc::C, true, false>> {
            Builder {
                state: CCState {
                    target: target as *mut _,
                    cc: cc::C,
                },
            }
        }

        pub fn cc<CC>(self, cc: CC) -> Builder<CCState<CC, false, true>> {
            Builder {
                state: CCState {
                    target: core::ptr::null_mut(),
                    cc,
                },
            }
        }
    }

    impl<CC, const HAS_CC: bool> Builder<CCState<CC, false, HAS_CC>> {
        pub fn target_ptr(self, target: *mut *const ()) -> Builder<CCState<CC, true, HAS_CC>> {
            Builder {
                state: CCState {
                    target,
                    cc: self.state.cc,
                },
            }
        }

        pub fn target_addr(self, target: usize) -> Builder<CCState<CC, true, HAS_CC>> {
            Builder {
                state: CCState {
                    target: target as *mut _,
                    cc: self.state.cc,
                },
            }
        }
    }

    impl<const HAS_TARGET: bool> Builder<CCState<cc::C, HAS_TARGET, false>> {
        pub fn cc<CC>(self, cc: CC) -> Builder<CCState<CC, HAS_TARGET, true>> {
            Builder {
                state: CCState {
                    target: self.state.target,
                    cc,
                },
            }
        }
    }

    impl<CC, const HAS_CC: bool> Builder<CCState<CC, true, HAS_CC>> {
        pub fn hook<'a, B: FnPtr, F>(
            self,
            hook: F,
        ) -> Builder<ThunkState<'a, B, impl FnThunk<B> + Send + Sync + 'a>>
        where
            (CC, F): FnThunk<B> + Send + Sync + 'a,
        {
            Builder {
                state: ThunkState {
                    target: self.state.target,
                    thunk: (self.state.cc, hook),
                    ctx_orig_setter: None,
                    phantom: PhantomData,
                },
            }
        }

        pub fn hook_ctx<'a, B: FnPtr, H, F>(
            self,
            hook_getter: F,
        ) -> Builder<ThunkState<'a, B, impl FnThunk<B> + Send + Sync + 'a>>
        where
            F: FnOnce(Context<B>) -> H,
            (CC, H): FnThunk<B> + Send + Sync + 'a,
        {
            let context = Context {
                original: Arc::default(),
            };
            let original = context.original.clone();

            Builder {
                state: ThunkState {
                    target: self.state.target,
                    thunk: (self.state.cc, hook_getter(context)),
                    ctx_orig_setter: Some(original),
                    phantom: PhantomData,
                },
            }
        }
    }

    impl<B: FnPtr> Builder<SigState<B>> {
        pub fn hook<'a, F>(
            self,
            hook: F,
        ) -> Builder<ThunkState<'a, B, impl FnThunk<B> + Send + Sync + 'a>>
        where
            (B::CC, F): FnThunk<B> + Send + Sync + 'a,
        {
            Builder {
                state: ThunkState {
                    target: self.state.target.cast(),
                    thunk: (B::CC::default(), hook),
                    ctx_orig_setter: None,
                    phantom: PhantomData,
                },
            }
        }

        pub fn hook_ctx<'a, H, F>(
            self,
            hook_getter: F,
        ) -> Builder<ThunkState<'a, B, impl FnThunk<B> + Send + Sync + 'a>>
        where
            F: FnOnce(Context<B>) -> H,
            (B::CC, H): FnThunk<B> + Send + Sync + 'a,
        {
            let context = Context {
                original: Arc::default(),
            };
            let original = context.original.clone();

            Builder {
                state: ThunkState {
                    target: self.state.target.cast(),
                    thunk: (B::CC::default(), hook_getter(context)),
                    ctx_orig_setter: Some(original),
                    phantom: PhantomData,
                },
            }
        }
    }

    impl<B: FnPtr + 'static, T: FnThunk<B> + Send + Sync + 'static> Builder<ThunkState<'static, B, T>> {
        pub unsafe fn install(self) -> crate::Result<LeakyHook<'static, RawPtrHook, B>> {
            let state = self.state;

            let bare = BareFnSync::with_thunk(state.thunk);

            let mut raw = RawPtrHook {
                target: state.target,
                original: core::ptr::null(),
                hook: bare.bare().to_ptr(),
            };

            let original = unsafe { B::from_ptr(raw.swap_target(raw.hook)?) };
            raw.original = original.to_ptr();

            if let Some(orig_setter) = state.ctx_orig_setter {
                orig_setter.call_once(|| original);
            }

            Ok(LeakyHook::new(raw, bare))
        }

        pub unsafe fn install_toggleable(
            self,
        ) -> crate::Result<ToggleableHook<'static, RawPtrHook, B>> {
            let state = self.state;
            let thunk = state.thunk;

            let orig_setter = state.ctx_orig_setter.unwrap_or_default();
            let orig_for_toggle = orig_setter.clone();

            let owned_switch = Arc::new(AtomicBool::new(true));
            let weak_switch = Arc::downgrade(&owned_switch);

            let toggleable = thunk_factory::make_send_sync(move |args| {
                if weak_switch
                    .upgrade()
                    .is_some_and(|enabled| enabled.load(Ordering::Acquire))
                {
                    unsafe { thunk.call(args) }
                }
                else {
                    unsafe { orig_for_toggle.wait().call(args) }
                }
            });

            let bare: BareFnSync<'_, B> = BareFnSync::with_thunk(toggleable);

            let mut raw = RawPtrHook {
                target: state.target,
                original: core::ptr::null(),
                hook: bare.bare().to_ptr(),
            };

            let original = unsafe { B::from_ptr(raw.swap_target(raw.hook)?) };
            raw.original = original.to_ptr();
            orig_setter.call_once(|| original);

            Ok(ToggleableHook {
                raw,
                enabled: owned_switch,
                _bare: ManuallyDrop::new(bare),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use closure_ffi::cc;

    use super::builder::Builder;

    extern "C" fn fun1(arg: usize) -> usize {
        arg + 123
    }

    extern "C" fn fun2(arg: usize) -> usize {
        2 * arg
    }

    static VMT_OR_IAT: &[unsafe extern "C" fn(usize) -> usize] = &[fun1, fun2];

    #[test]
    fn test_non_toggleable_hook_by_sig() {
        let builder = Builder::new()
            .target_fn((&raw const VMT_OR_IAT[0]).cast_mut())
            .hook_ctx(|ctx| move |n| n + unsafe { ctx.original_ptr()(n) });

        let _hook0 = unsafe { builder.install() }.unwrap();
        assert_eq!(unsafe { VMT_OR_IAT[0](100) }, 323);
    }

    #[test]
    fn test_toggleable_hook_ctx_by_addr() {
        let builder = Builder::new()
            .target_addr((&raw const VMT_OR_IAT[1]).addr())
            .cc(cc::C)
            .hook_ctx(|ctx| {
                move |n: usize| -> usize {
                    let original_value = unsafe { ctx.call_original((n,)) };
                    original_value + 1000
                }
            });

        let hook1 = unsafe { builder.install_toggleable() }.unwrap();

        assert_eq!(unsafe { VMT_OR_IAT[1](100) }, 1200);

        hook1.disable();
        assert_eq!(unsafe { VMT_OR_IAT[1](100) }, 200);

        hook1.enable();
        assert_eq!(unsafe { VMT_OR_IAT[1](50) }, 1100);

        drop(hook1); // should disable
        assert_eq!(unsafe { VMT_OR_IAT[1](200) }, 400);
    }
}
