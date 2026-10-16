//! Драйвер virtio-blk. Предоставляет протокол `BlockDevice` диска первого транспорта
//! virtio-blk и обслуживает одну сессию диска одновременно.

#![no_std]
#![no_main]

extern crate alloc;

mod device;
mod entry;
