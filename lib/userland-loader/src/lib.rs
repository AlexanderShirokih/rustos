//! Загрузка программы из её [`ProgramView`](userland::ProgramView) в новый процесс из userspace.
//!
//! [`load_program`] создаёт процесс программы и передаёт ему одну начальную capability. Сегменты
//! без записи программы из переданных [`ImagePages`] процесс получает срезами региона образа без
//! расхода своего бюджета. Остальные сегменты загрузчик копирует, а стек создаёт в регионах,
//! оплаченных ресурсом вызывающего. Отказ после создания процесса завершает процесс.

#![no_std]

extern crate alloc;

mod image_pages;
mod load;

pub use image_pages::ImagePages;
pub use load::load_program;
