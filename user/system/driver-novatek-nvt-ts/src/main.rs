//! Драйвер тачскрина Novatek NT36672A на шине I2C. Экспортирует `InputSource` и передаёт
//! касания хабу ввода. Для работы требуется питание чипа, включённое загрузчиком.

#![no_std]
#![no_main]

extern crate alloc;

mod driver;
mod entry;
