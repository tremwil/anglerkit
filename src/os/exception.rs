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
    /// # Panics
    /// Depending on the Rust panic implementation on the current platform, whenever
    /// `fun` panics the method may either panic normally or swallow the panic and
    /// report it as an exception.
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
    /// # Panics
    /// Depending on the Rust panic implementation on the current platform, whenever
    /// `fun` panics the method may either panic normally or swallow the panic and
    /// report it as an exception.
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

            struct CCThunkData {
                ctx: *mut (),
                fun: unsafe fn(*mut ()),
            }

            #[unsafe(naked)]
            #[unsafe(link_section = ".text")]
            unsafe extern "C" fn try_except_seh(
                ctx: &CCThunkData,
                fun: unsafe extern "C-unwind" fn(&CCThunkData),
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

            unsafe extern "C-unwind" fn thunk(ctx: &CCThunkData) {
                unsafe { (ctx.fun)(ctx.ctx) }
            }
            unsafe { try_except_seh(&CCThunkData { ctx, fun }, thunk) }
        }

        // On other envs, use a diy setjmp based on `AddVectoredExceptionHandler` and a thread
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

            #[unsafe(naked)]
            unsafe extern "C" fn veh_setjmp() -> bool {
                #[cfg(target_arch = "x86")]
                core::arch::naked_asm!("int3", "ret");
                #[cfg(target_arch = "aarch64")]
                core::arch::naked_asm!("brk", "ret");
            }

            unsafe extern "system" fn veh(ex_ptr: *mut EXCEPTION_POINTERS) -> i32 {
                let ctx = unsafe { &mut *(*ex_ptr).ContextRecord };
                let ex_info = unsafe { &*(*ex_ptr).ExceptionRecord };

                // veh_setjmp was called. Skip the breakpoint and save the context,
                // returning false
                if ex_info.ExceptionAddress.addr() == veh_setjmp as usize {
                    #[cfg(target_arch = "x86")]
                    {
                        ctx.Eip += 1;
                        ctx.Eax = 0;
                    }
                    #[cfg(target_arch = "aarch64")]
                    {
                        ctx.Pc += 4;
                        ctx.Anonymous.X0 = 0;
                    }
                    CONTEXT_STACK.with_borrow_mut(|stack| stack.push(*ctx));
                    return EXCEPTION_CONTINUE_EXECUTION;
                }
                CONTEXT_STACK.with_borrow_mut(|stack| {
                    let Some(saved_ctx) = stack.last_mut()
                    else {
                        // the veh was likely triggered from another thread due to some unrelated
                        // exception. Forward to the next handler
                        return EXCEPTION_CONTINUE_SEARCH;
                    };
                    // Restore the context while setting the return value of veh_setjmp to true
                    #[cfg(target_arch = "x86")]
                    let ret = &mut saved_ctx.Eax;
                    #[cfg(target_arch = "aarch64")]
                    let ret = saved_ctx.Anonymous.X0;
                    *ret = 1;
                    *ctx = *saved_ctx;

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

            let second_return = unsafe { veh_setjmp() };
            if !second_return {
                unsafe { fun(ctx) };
            }

            unsafe { RemoveVectoredExceptionHandler(veh_handle) };

            // For some reason (probably missing the returns_twice llvm attr on veh_setjmp),
            // the codegen is broken and second_return always evaluates to false (despite the code
            // in the if running once). So we read it from the context instead
            let ctx = CONTEXT_STACK.with_borrow_mut(|stack| stack.pop().unwrap());
            #[cfg(target_arch = "x86")]
            return ctx.Eax == 0;
            #[cfg(target_arch = "aarch64")]
            return ctx.Anonymous.X0 == 0;
        }
    }

    #[cfg(test)]
    mod tests {
        use crate::os::{OsImpl, exception::ExceptionFunctionsEx};

        #[test]
        #[cfg(any(target_os = "windows"))]
        fn test_seh() {
            let result = OsImpl.try_except(|| unsafe {
                std::ptr::null_mut::<u8>().write_volatile(0);
            });
            assert!(result.is_err())
        }
    }
}
