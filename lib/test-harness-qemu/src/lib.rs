//! Каркас интеграционных тестов ядра, исполняемых под эмулятором.

#![no_std]

#[cfg(test)]
extern crate std;

pub mod case;
pub mod macros;
pub mod runner;

pub use case::TestCase;
pub use runner::run_all_tests;
pub use test_harness_macros::kernel_test;
