//! Драйверы устройств AArch64.

#![no_std]

#[cfg(target_os = "none")]
extern crate alloc;

#[cfg(target_os = "none")]
mod commons;
#[cfg(target_os = "none")]
pub mod pl011_uart;
#[cfg(target_os = "none")]
pub mod qcom_uart_dm;
