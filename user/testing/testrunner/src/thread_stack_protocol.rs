//! Протокол фикстуры, у которой поток переполняет свой стек, и коды её выхода.

// Модуль подключён к обоим бинарям, и каждый пользуется своим подмножеством.
#![allow(dead_code)]

use core::sync::atomic::AtomicU64;

/// Размер стека потока фикстуры в байтах.
pub const THREAD_STACK_BYTES: u64 = 0x4000;

/// Страница отчёта не отобразилась.
pub const MAP_FAILED_EXIT_CODE: u32 = 1;
/// Поток не запустился.
pub const SPAWN_FAILED_EXIT_CODE: u32 = 2;
/// Поток вернулся из рекурсии.
pub const RECURSION_RETURNED_EXIT_CODE: u32 = 3;
/// Ожидание потока отказало.
pub const JOIN_FAILED_EXIT_CODE: u32 = 4;

/// Отчёт потока в начале разделяемой страницы.
#[repr(C)]
pub struct StackReport {
    /// Адрес локальной переменной на входе в тело потока.
    pub entry_frame: AtomicU64,
    /// Адрес буфера самого глубокого кадра рекурсии, который поток успел записать.
    pub deepest_frame: AtomicU64,
}
