use crate::liballoc::{borrow::Cow, string::String};

pub mod memory;

/// An error from the operating system represented as a human-readable string.
///
/// Although these errors are not meant to be recoverable in the context of this crate,
/// it is sometimes safer to ignore them instead of panicking.
#[derive(Debug, Clone)]
pub struct OsErr(Cow<'static, str>);

impl OsErr {
    pub fn from_str(error: &'static str) -> Self {
        OsErr(Cow::Borrowed(error))
    }

    pub fn from_string(error: String) -> Self {
        OsErr(Cow::Owned(error))
    }
}

impl core::fmt::Display for OsErr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "OS error: {}", self.0)
    }
}

impl core::error::Error for OsErr {}

/// Dyn-compatible trait used to provide implementations of OS functions required by the
/// crate in a cross-platform and `no_std` friendly manner.
pub trait OsFunctions: memory::MemFunctions {}

/// Marker type which defers to the current [`OsFunctions`] implementation used by
/// anglerkit.
///
/// When the `std` feature is enabled, the functions are implemented using APIs from
/// `std` and the [region] crate. When disabled, the implementation must be provided by
/// the user through the [`os_api_impl!`] macro.
pub(crate) struct OsImpl;

impl OsFunctions for OsImpl {}

#[cfg(not(feature = "std"))]
mod no_std {
    use super::*;

    unsafe extern "Rust" {
        fn anglerkit_v0_os_impl() -> &'static (dyn OsFunctions + Sync);
    }

    pub fn os_impl() -> &'static (dyn OsFunctions + Sync) {
        unsafe { anglerkit_v0_os_impl() }
    }
}

/// Specify the [`OsFunctions`] implementation that will be used by the crate to perform
/// certain OS-specific tasks, e.g. virtual memory protection.
///
/// The macro accepts a path to a static variable, zero-sized-type or an unsafe block
/// resolving to a `&'static (dyn OsFunctions + Sync)`.
///
/// # Examples
///
/// Stateful const-constructible implementation:
/// ```ignore
/// static OS_FUNCS: OsFunctions = MyOsFunctions::new();
/// os_api_impl!(OS_FUNCS);
/// ```
///
/// Stateless (ZST) implementation:
/// ```ignore
/// os_api_impl!(MyOsFunctions);
/// ```
///
/// Stateful, runtime (lazily) initialized implementation:
/// ```ignore
/// use std::sync::OnceLock;
///
/// // SAFETY: The block always evaluates to the same instance of MyOsFunctions.
/// os_api_impl!(unsafe {
///     static WRAPPED: OnceLock<MyOsFunctions> = OnceLock::new();
///     WRAPPED.get_or_init(|| MyOsFunctions::new())
/// });
/// ```
///
/// # Safety
/// The block form must be marked with `unsafe` as returning a different impl may be
/// unsound depending on the implementor's internal state. You are responsible to
/// make sure this doesn't happen.
#[cfg(any(doc, not(feature = "std")))]
#[macro_export]
macro_rules! os_api_impl {
    ($static_var:path) => {
        #[unsafe(no_mangle)]
        extern "Rust" fn anglerkit_v0_os_impl() -> &'static (dyn OsFunctions + Sync) {
            &$static_var
        }
    };
    (unsafe $provider:block) => {
        #[unsafe(no_mangle)]
        extern "Rust" fn anglerkit_v0_os_impl() -> &'static (dyn OsFunctions + Sync) {
            unsafe { $static_var }
        }
    };
}
