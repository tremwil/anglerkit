#[cfg(target_arch = "aarch64")]
pub mod aarch64;

#[cfg(target_arch = "x86_64")]
pub mod x86_64;

#[cfg(target_arch = "x86")]
pub mod x86;

#[cfg(all(target_arch = "arm", thumb_mode))]
pub mod a32;

#[cfg(all(target_arch = "arm", thumb_mode))]
pub mod t32;
