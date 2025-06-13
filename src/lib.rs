#![cfg_attr(feature = "no_std", no_std)]
#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(feature = "no_std")]
extern crate alloc as _liballoc;
#[cfg(not(feature = "no_std"))]
use std as liballoc;

#[cfg(feature = "no_std")]
use _liballoc as liballoc;

pub mod traits;

pub mod trampoline;

pub mod alloc;
