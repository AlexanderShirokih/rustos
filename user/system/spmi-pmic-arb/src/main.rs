//! Шина устройств PMIC за арбитром SPMI `qcom,spmi-pmic-arb`. Предоставляет протокол `Spmi`
//! с сессиями `SpmiDevice` к регистрам устройств и сессиями `SpmiInterrupt` к их прерываниям.

#![no_std]
#![no_main]

extern crate alloc;

mod bus;
mod entry;
mod windows;
