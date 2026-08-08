#![cfg_attr(target_os = "none", no_std)]

#[cfg(not(target_os = "none"))]
mod host;
#[cfg(not(target_os = "none"))]
mod manifest;
#[cfg(not(target_os = "none"))]
mod mounts;
#[cfg(not(target_os = "none"))]
mod plan;
#[cfg(not(target_os = "none"))]
mod seeds;
#[cfg(not(target_os = "none"))]
mod units;

#[cfg(not(target_os = "none"))]
pub use host::{BuildEnvironment, ExtraFile, build_userland};
#[cfg(not(target_os = "none"))]
pub use seeds::{ImageSeed, userland_image_seeds};
