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

#[cfg(all(feature = "std", windows))]
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
                    RtlCaptureContext,
                },
            };

            #[derive(Default)]
            struct ExFrame {
                ctx: CONTEXT,
                had_exception: bool,
            }

            thread_local! {
                static CONTEXT_STACK: RefCell<Vec<ExFrame>> = const { RefCell::new(Vec::new()) };
            }

            unsafe extern "system" fn veh(ex_ptr: *mut EXCEPTION_POINTERS) -> i32 {
                let ctx = unsafe { &mut *(*ex_ptr).ContextRecord };

                CONTEXT_STACK.with_borrow_mut(|stack| {
                    let Some(frame) = stack.last_mut()
                    else {
                        // the veh was likely triggered from another thread due to some unrelated
                        // exception. Forward to the next handler
                        return EXCEPTION_CONTINUE_SEARCH;
                    };
                    // Notify of the exception and restore the context to the savepoint
                    frame.had_exception = true;
                    *ctx = frame.ctx;
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

            // push an empty context. This has to be done before RtlCaptureContext
            CONTEXT_STACK.with_borrow_mut(|stack| stack.push(ExFrame::default()));

            // Use a drop guard to make sure that critical cleanup is performed even if a panic from
            // `fun` is not caught by the veh
            struct VehDropGuard(*mut core::ffi::c_void);
            impl Drop for VehDropGuard {
                fn drop(&mut self) {
                    unsafe { RemoveVectoredExceptionHandler(self.0) };
                    CONTEXT_STACK.with_borrow_mut(|stack| {
                        stack.pop().expect("try_except_raw context stack underflow")
                    });
                }
            }
            let drop_guard = VehDropGuard(veh_handle);

            // capture the current context. If an exception occurs, the VEH will restore the
            // thread context to this point and we will be able to check had_exception.
            let mut context = CONTEXT::default();
            unsafe { RtlCaptureContext(&mut context) };

            let had_exception = CONTEXT_STACK.with_borrow_mut(|stack| {
                let frame = stack.last_mut().expect("try_except_raw context stack is empty");
                if !frame.had_exception {
                    frame.ctx = context;
                }
                frame.had_exception
            });

            if !had_exception {
                unsafe { fun(ctx) };
            }

            drop(drop_guard);
            return !had_exception;
        }
    }
}

// On unix systems, implementing this is inherently problematic due to the lack of an
// API for registering and unregistering hardware exception handlers from multiple
// non-coordinating libraries/threads without incurring race conditions.
//
// As such, the implementation is feature-gated and by default we don't provide
// thread safety guarantees when hooking over non-RWX memory.
//
// When the feature is set, the implementation uses a signal handler configured to catch
// hardware exceptions which context switches back to the function using `siglongjmp`.
// This is fine as long as no foreign code register the same signal handlers.
#[cfg(all(feature = "std", unix))]
mod unix {
    use crate::os::{OsImpl, exception::ExceptionFunctions};

    impl ExceptionFunctions for OsImpl {
        #[cfg(feature = "unix_try_except")]
        unsafe fn try_except_raw(&self, ctx: *mut (), fun: unsafe fn(*mut ())) -> bool {
            use core::{
                cell::RefCell,
                ffi::c_int,
                mem::{self, MaybeUninit},
            };
            use std::{sync::Once, vec::Vec};

            use libc::{SIGBUS, SIGFPE, SIGILL, SIGSEGV, SIGTRAP, sigaction};
            use setjmp::{jmp_buf, siglongjmp, sigsetjmp};

            std::thread_local! {
                static CONTEXT_STACK: RefCell<Vec<jmp_buf>> = const { RefCell::new(Vec::new()) };
            }

            const HARDWARE_SIGNALS: &[i32] = &[SIGILL, SIGFPE, SIGSEGV, SIGBUS, SIGTRAP];

            unsafe extern "system" fn sa_handler(_signal: c_int) {
                if let Some(mut jmp_buf) =
                    CONTEXT_STACK.with_borrow_mut(|stack| stack.last_mut().copied())
                {
                    unsafe { siglongjmp(&mut jmp_buf, 1) };
                }
            }

            static REGISTER_HANDLER: Once = Once::new();
            REGISTER_HANDLER.call_once(|| {
                let act = sigaction {
                    sa_sigaction: sa_handler as usize,
                    sa_mask: unsafe { mem::zeroed() },
                    sa_flags: 0,
                    sa_restorer: None,
                };
                for &s in HARDWARE_SIGNALS {
                    let mut oldact = MaybeUninit::uninit();
                    if unsafe { sigaction(s, &act, oldact.as_mut_ptr()) } != 0 {
                        panic!("sigaction failed: {}", std::io::Error::last_os_error());
                    }
                }
            });

            let mut env = unsafe { mem::zeroed() };
            let had_exception = unsafe { sigsetjmp(&mut env, 1) };

            if had_exception == 0 {
                CONTEXT_STACK.with_borrow_mut(|stack| stack.push(env));
            }
            struct DropGuard;
            impl Drop for DropGuard {
                fn drop(&mut self) {
                    CONTEXT_STACK.with_borrow_mut(|stack| stack.pop().unwrap());
                }
            }
            let drop_guard = DropGuard;

            if had_exception == 0 {
                unsafe { fun(ctx) };
            }

            drop(drop_guard);
            return had_exception == 0;
        }
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use crate::os::{OsImpl, exception::ExceptionFunctionsEx};

    #[test]
    #[cfg(any(windows, feature = "unix_try_except"))]
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
