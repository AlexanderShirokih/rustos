//! Контроллер выводов TLMM SDM660 `qcom,sdm660-pinctrl`. Предоставляет протокол `Pins`:
//! применяет к выводам узла состояние `default` и держит их сессией `DevicePins`, которая
//! выдаёт линии выводов и их прерывания.

#![no_std]
#![no_main]

extern crate alloc;

mod entry;
mod hardware;
mod sessions;
mod windows;
