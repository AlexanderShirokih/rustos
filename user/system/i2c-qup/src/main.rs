//! Шина устройств I2C за контроллерами QUP v2 `qcom,i2c-qup-v2.2.1`. Предоставляет протокол
//! `I2c` и сессии `I2cDevice` к передачам устройств.

#![no_std]
#![no_main]

extern crate alloc;

mod entry;
mod sessions;
mod supplies;
