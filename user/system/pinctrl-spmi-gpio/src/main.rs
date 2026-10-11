//! Контроллер GPIO PMIC Qualcomm `qcom,spmi-gpio`. Предоставляет протокол `Pins`: применяет к
//! выводам узла состояние `default`, держит их сессией `DevicePins` и выдаёт линии-входы с
//! прерываниями.

#![no_std]
#![no_main]

extern crate alloc;

mod device;
mod entry;
mod sessions;
