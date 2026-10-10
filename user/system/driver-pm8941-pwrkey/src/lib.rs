//! Драйвер кнопок блока PON PMIC Qualcomm `qcom,pm8941-pwrkey` и `qcom,pm8941-resin` за шиной
//! SPMI. Каждая кнопка - отдельная программа и источник ввода `InputSource` с одной клавишей.
#![no_std]

extern crate alloc;

mod driver;
mod key;

pub use driver::{Stopped, run};
pub use key::{Generation, Key, PWRKEY, RESIN, usage};
