//! Протокол фикстуры, у которой поток переполняет свой стек, и коды её выхода.

// Модуль подключён к обоим бинарям, и каждый пользуется своим подмножеством.
#![allow(dead_code)]

use core::sync::atomic::AtomicU64;

pub(crate) const THREAD_STACK_BYTES: u64 = 0x4000;

pub(crate) const MAP_FAILED_EXIT_CODE: u32 = 1;
pub(crate) const SPAWN_FAILED_EXIT_CODE: u32 = 2;
pub(crate) const RECURSION_RETURNED_EXIT_CODE: u32 = 3;
pub(crate) const JOIN_FAILED_EXIT_CODE: u32 = 4;

#[repr(C)]
pub(crate) struct StackReport {
    /// Адрес локальной переменной на входе в тело потока.
    pub entry_frame: AtomicU64,
    /// Адрес буфера самого глубокого кадра рекурсии, который поток успел записать.
    pub deepest_frame: AtomicU64,
}
