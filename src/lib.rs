#![cfg_attr(not(feature = "std"), no_std)]
#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(not(feature = "std"))]
extern crate alloc as _liballoc;
#[cfg(feature = "std")]
use std as liballoc;

#[cfg(not(feature = "std"))]
use _liballoc as liballoc;

pub mod traits;

pub mod trampoline;

pub mod alloc;
