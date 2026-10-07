//! Драйвер virtio-gpu. Предоставляет протокол `Display` экрана GPU, слот транспорта которого
//! выдаёт шина `VirtioMmio`.

#![no_std]
#![no_main]

extern crate alloc;

mod device;
mod entry;
mod memory;
mod registers;
