//! Логика контроллера шины I2C QUP v2 Qualcomm (`qcom,i2c-qup-v2.2.1`) в режиме FIFO:
//! устройства за контроллером по описанию платформы, подготовка контроллера и передачи по
//! `drivers/i2c/busses/i2c-qup.c` Linux 6.16.
#![no_std]

extern crate alloc;

mod bus;
mod controller;
mod registers;

pub use bus::{Bus, Device, Failure, Resources};
pub use controller::{Controller, Line, Operation, Refusal};
