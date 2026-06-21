#![cfg_attr(not(test), no_std)]

pub mod boot;
pub mod scanner;

#[cfg(test)]
mod test_util;
