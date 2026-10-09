//! Шина устройств PMIC за арбитром SPMI `qcom,spmi-pmic-arb`. Предоставляет протокол `Spmi`
//! и сессии `SpmiDevice` к регистрам и прерываниям устройств.

#![no_std]
#![no_main]

extern crate alloc;

mod bus;
mod entry;
mod windows;
