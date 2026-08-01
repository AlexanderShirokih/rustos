#![cfg_attr(not(test), no_std)]

pub mod boot;
pub mod cpu;
pub mod scanner;
pub mod smp;

#[cfg(test)]
mod test_util;
