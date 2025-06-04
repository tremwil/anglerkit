use closure_ffi::traits::FnPtr;

trait RawHook {
    fn new(target: impl Into<*const ()>) -> Self;

    fn original(&self) -> *const ();

    unsafe fn set_hook(&self, hook: *const ());

    unsafe fn enable(&self);

    unsafe fn disable(&self);
}

trait HookStrategy {
    type Context<'a, B: FnPtr>;
}
