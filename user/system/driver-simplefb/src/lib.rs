//! Буфер кадра `simple-framebuffer`, который настроил загрузчик.
//!
//! [`Framebuffer::open`] читает размер экрана и шаг строки из узла устройства.
//! [`Framebuffer::present`] копирует прямоугольник поверхности `Display` в буфер кадра.
//! Операции чтения поверхности и записи экрана передаёт вызывающий.
#![no_std]

mod framebuffer;

pub use framebuffer::{Framebuffer, Refusal, SIMPLE_FRAMEBUFFER_COMPATIBLE};
