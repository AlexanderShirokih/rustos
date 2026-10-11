//! Контроллер выводов TLMM SDM660 `qcom,sdm660-pinctrl`. Предоставляет протокол `Pins`:
//! применяет к выводам узла состояние `default` и держит их сессией `DevicePins`.

#![no_std]
#![no_main]

extern crate alloc;

mod entry;
mod sessions;
mod windows;
