//! Логика арбитра SPMI PMIC Qualcomm (`qcom,spmi-pmic-arb`) версий 2 и 3: команды к регистрам
//! устройств PMIC, прерывания каналов и разбор устройств за арбитром по описанию платформы.
#![no_std]

extern crate alloc;

mod arbiter;
mod peripheral;
mod registers;

pub use arbiter::{Arbiter, Refusal, Version};
pub use peripheral::{ArbiterNode, Interrupt, NodeError, Peripheral, SkipReason, Skipped, Span};
pub use registers::Window;
