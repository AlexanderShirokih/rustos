#![cfg_attr(target_os = "none", no_std)]

#[cfg(not(target_os = "none"))]
mod host;

#[cfg(not(target_os = "none"))]
pub use host::build_userland;
