//! Компонент для проверки приёма вызовов и экспорта во время ожидания соседнего сервера.
#![no_std]
#![no_main]

extern crate alloc;

mod async_component_protocol;
mod failed;
#[path = "async_component_fixture/service.rs"]
mod service;
