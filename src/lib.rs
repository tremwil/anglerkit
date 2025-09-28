#![no_std]
#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]
#![cfg_attr(feature = "nightly", feature(unboxed_closures, fn_traits))]
#![deny(unsafe_op_in_unsafe_fn)]

extern crate alloc as liballoc;

#[cfg(feature = "std")]
extern crate std;

pub mod traits;

pub mod trampoline;

pub mod alloc;

pub mod os;

pub mod hook;

pub mod error;

#[doc(inline)]
pub use error::{Error, Result};
