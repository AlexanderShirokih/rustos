#![cfg_attr(target_os = "none", no_std)]

#[cfg(not(target_os = "none"))]
mod host;
#[cfg(not(target_os = "none"))]
mod seeds;

#[cfg(not(target_os = "none"))]
pub use host::{BuildEnvironment, build_userland};
#[cfg(not(target_os = "none"))]
pub use seeds::{ImageSeed, userland_image_seeds};
