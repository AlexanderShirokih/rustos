//! Контроллер тактирования GCC SDM660 `qcom,gcc-sdm660`. Предоставляет протокол `Clocks`:
//! включает такты узла и держит узел сессией `DeviceClocks`, которая задаёт частоту такта.

#![no_std]
#![no_main]

extern crate alloc;

mod entry;
mod sessions;
mod window;
