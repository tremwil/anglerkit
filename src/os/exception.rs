use core::mem::MaybeUninit;

pub trait ExceptionFunctions {
    /// Execute a bare function taking some type-erased context within a hardware
    /// exception handler.
    ///
    /// Returns `true` if the function executed successfully and `false` if execution
    /// was interrupted due to an exception (e.g. `EXCEPTION_ACCESS_VIOLATION` or
    /// `SIGBUS`).
    ///
    /// This method exists to preserve dyn compatibility while allowing arbitrary
    /// closures to be invoked without unnecessary boxing. For a safe, ergonomic API
    /// consider using [`ExceptionFunctionsEx::try_except`].
    ///
    /// # Panic Propagation
    /// Depending on the Rust panic implementation on the current platform, whenever
    /// `fun` panics any of these (safe) behaviors may be observed:
    /// - The panic is propagated normally.
    /// - The panic is swallowed and try_except_raw returns `false` (`catch_unwind`-like
    ///   behavior).
    /// - The process aborts.
    ///
    /// As such, `fun` should not panic or attempt to catch panics.
    ///
    /// # Safety
    /// - Unsafe function `fun` must be valid to call with `ctx`.
    /// - Due to platform and architecture differences, this method only catches
    ///   exceptions on a best-effort basis. Its use should be limited to best-effort
    ///   recovery from hardware exceptions, and program soundness should not be based
    ///   on this method's ability to catch a particular exception.
    unsafe fn try_except_raw(&self, ctx: *mut (), fun: unsafe fn(*mut ())) -> bool {
        unsafe { fun(ctx) };
        true
    }
}

/// Extension trait for [`ExceptionFunctions`] containing non dyn-compatible methods.
pub trait ExceptionFunctionsEx: ExceptionFunctions {
    /// Execute a closure within a hardware exception handler.
    ///
    /// Returns [`Ok`] if the function executed successfully and [`Err`] if execution
    /// was interrupted due to an exception (e.g. `EXCEPTION_ACCESS_VIOLATION` or
    /// `SIGBUS`).
    ///
    /// # Panic Propagation
    /// Depending on the Rust panic implementation on the current platform, whenever
    /// `fun` panics any of these (safe) behaviors may be observed:
    /// - The panic is propagated normally.
    /// - The panic is swallowed and try_except_raw returns `false` (`catch_unwind`-like
    ///   behavior).
    /// - The process aborts.
    ///
    /// As such, `fun` should not panic or attempt to catch panics.
    ///
    /// # Safety
    /// Due to platform and architecture differences, this method only catches
    /// exceptions on a best-effort basis. Do not rely on it catching any particular
    /// exception for soundness.
    fn try_except<F: FnOnce() -> R, R>(&self, fun: F) -> Result<R, ()> {
        struct Ctx<F, R> {
            fun: MaybeUninit<F>,
            ret: MaybeUninit<R>,
        }

        let mut ctx = Ctx {
            fun: MaybeUninit::new(fun),
            ret: MaybeUninit::uninit(),
        };

        unsafe {
            self.try_except_raw(&raw mut ctx as *mut (), |ctx| {
                // SAFETY: `ctx` type matches, borrow is valid for this lifetime
                let ctx = &mut *ctx.cast::<Ctx<F, R>>();
                // SAFETY: `ctx.ftpr` was initialized to `fun`
                let fun = ctx.fun.assume_init_read();
                ctx.ret.write(fun());
            })
            // SAFETY: by contract of `try_except_raw`, if true is returned then the function
            // has fully executed. Hence fun's return value was written to `ctx.ret`
            .then(|| ctx.ret.assume_init())
            .ok_or(())
        }
    }
}

impl<T: ExceptionFunctions + ?Sized> ExceptionFunctionsEx for T {}

#[cfg(all(feature = "std", target_os = "windows"))]
mod windows {
    use crate::os::{OsImpl, exception::ExceptionFunctions};

    impl ExceptionFunctions for OsImpl {
        // on msvc, use the microseh crate for cross-arch support
        #[cfg(target_env = "msvc")]
        unsafe fn try_except_raw(&self, ctx: *mut (), fun: unsafe fn(*mut ())) -> bool {
            microseh::try_seh(|| unsafe { fun(ctx) }).is_ok()
        }

        // On gnu x86_64, we can use SEH directives directly
        #[cfg(all(target_env = "gnu", target_arch = "x86_64"))]
        unsafe fn try_except_raw(&self, ctx: *mut (), fun: unsafe fn(*mut ())) -> bool {
            use core::arch::naked_asm;

            use windows_sys::Win32::System::Diagnostics::Debug::EXCEPTION_EXECUTE_HANDLER;

            struct CCThunkData {
                ctx: *mut (),
                fun: unsafe fn(*mut ()),
            }

            #[unsafe(naked)]
            #[unsafe(link_section = ".text")]
            unsafe extern "C" fn try_except_seh(
                ctx: &CCThunkData,
                fun: unsafe extern "C" fn(&CCThunkData),
            ) -> bool {
                naked_asm!(
                    ".seh_proc {fct_name}",
                    "sub rsp, 0x28",
                    ".seh_stackalloc 0x28",
                    ".seh_endprologue",
                    ".seh_handler __C_specific_handler, @except",
                    ".seh_handlerdata",
                    ".long 1",
                    ".long (2f)@IMGREL",
                    ".long (3f)@IMGREL",
                    ".long {handler}",
                    ".long (4f)@IMGREL",
                    ".text",
                    "2:",
                    "call rdx",
                    "nop",
                    "3:",
                    "add rsp, 0x28",
                    "mov al, 1",
                    "ret",
                    "4:",
                    "add rsp, 0x28",
                    "mov al,0",
                    "ret",
                    ".seh_endproc",
                    fct_name = sym try_except_seh,
                    handler = const EXCEPTION_EXECUTE_HANDLER,
                );
            }

            unsafe extern "C" fn thunk(ctx: &CCThunkData) {
                unsafe { (ctx.fun)(ctx.ctx) }
            }
            unsafe { try_except_seh(&CCThunkData { ctx, fun }, thunk) }
        }

        // On other envs, use a DIY setjmp based on `AddVectoredExceptionHandler` and a thread
        // local stack of CONTEXTs
        #[cfg(not(any(target_env = "msvc", target_arch = "x86_64")))]
        unsafe fn try_except_raw(&self, ctx: *mut (), fun: unsafe fn(*mut ())) -> bool {
            use std::{cell::RefCell, thread_local, vec::Vec};

            use windows_sys::Win32::{
                Foundation::GetLastError,
                System::Diagnostics::Debug::{
                    AddVectoredExceptionHandler, CONTEXT, EXCEPTION_CONTINUE_EXECUTION,
                    EXCEPTION_CONTINUE_SEARCH, EXCEPTION_POINTERS, RemoveVectoredExceptionHandler,
                },
            };

            thread_local! {
                static CONTEXT_STACK: RefCell<Vec<CONTEXT>> = const { RefCell::new(Vec::new()) };
            }

            // use a breakpoint at a known address to trigger the VEH and save the context
            #[unsafe(naked)]
            unsafe extern "C" fn veh_setjmp() -> bool {
                #[cfg(target_arch = "x86")]
                core::arch::naked_asm!("int3");
                #[cfg(target_arch = "aarch64")]
                core::arch::naked_asm!("brk");
            }

            unsafe extern "system" fn veh(ex_ptr: *mut EXCEPTION_POINTERS) -> i32 {
                let ctx = unsafe { &mut *(*ex_ptr).ContextRecord };
                let ex_info = unsafe { &*(*ex_ptr).ExceptionRecord };

                // veh_setjmp was called. Return from it and save the context
                if ex_info.ExceptionAddress.addr() == veh_setjmp as usize {
                    #[cfg(target_arch = "x86")]
                    {
                        let return_address = unsafe { *(ctx.Esp as *const u32) };
                        ctx.Esp += 4;
                        ctx.Eip = return_address;
                        ctx.Eax = 0;
                    }
                    #[cfg(target_arch = "aarch64")]
                    {
                        ctx.Pc = ctx.Anonymous.X30;
                        ctx.Anonymous.X0 = 0;
                    }
                    CONTEXT_STACK.with_borrow_mut(|stack| stack.push(*ctx));
                    return EXCEPTION_CONTINUE_EXECUTION;
                }
                CONTEXT_STACK.with_borrow(|stack| {
                    let Some(saved_ctx) = stack.last()
                    else {
                        // the veh was likely triggered from another thread due to some unrelated
                        // exception. Forward to the next handler
                        return EXCEPTION_CONTINUE_SEARCH;
                    };
                    // Restore the context while setting the return value of veh_setjmp to true
                    *ctx = *saved_ctx;
                    #[cfg(target_arch = "x86")]
                    let ret = &mut ctx.Eax;
                    #[cfg(target_arch = "aarch64")]
                    let ret = &mut ctx.Anonymous.X0;
                    *ret = 1;

                    EXCEPTION_CONTINUE_EXECUTION
                })
            }

            let veh_handle = unsafe { AddVectoredExceptionHandler(1, Some(veh)) };
            if veh_handle.is_null() {
                panic!(
                    "AddVectoredExceptionHandler failed (err = 0x{:x})",
                    unsafe { GetLastError() }
                )
            }

            // Use a drop guard to make sure that critical cleanup is performed even if a panic from
            // `fun` is not caught by the veh
            struct VehDropGuard(*mut core::ffi::c_void);
            impl Drop for VehDropGuard {
                fn drop(&mut self) {
                    unsafe { RemoveVectoredExceptionHandler(self.0) };
                    CONTEXT_STACK.with_borrow_mut(|s| {
                        s.pop().expect("try_except_raw context stack underflow")
                    });
                }
            }
            let drop_guard = VehDropGuard(veh_handle);

            let had_exception = unsafe { veh_setjmp() };
            if !had_exception {
                unsafe { fun(ctx) };
            }

            drop(drop_guard);
            return !had_exception;
        }
    }

    #[cfg(all(test, feature = "std"))]
    mod tests {
        use crate::os::{OsImpl, exception::ExceptionFunctionsEx};

        #[test]
        fn test_exception() {
            let result = OsImpl.try_except(|| unsafe {
                std::println!("before exception");
                std::ptr::null_mut::<u8>().write_volatile(0);
                std::println!("after exception");
            });
            assert!(result.is_err());
        }

        #[test]
        fn test_no_exception() {
            let result = OsImpl.try_except(|| {
                std::println!("no exception");
            });
            assert!(result.is_ok());
        }
    }
}
