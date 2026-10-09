//! Протокол фикстуры, у которой поток переполняет свой стек, и коды её выхода.

// Модуль подключён к обоим бинарям, и каждый пользуется своим подмножеством.
#![allow(dead_code)]

use core::sync::atomic::AtomicU64;

use syscall::PAGE_SIZE;

pub(crate) const THREAD_STACK_BYTES: usize = 0x4000;

pub(crate) const SMALL_FRAME_BYTES: usize = 64;
pub(crate) const LARGE_FRAME_BYTES: usize = 3 * PAGE_SIZE;

pub(crate) const MAP_FAILED_EXIT_CODE: u32 = 1;
pub(crate) const SPAWN_FAILED_EXIT_CODE: u32 = 2;
pub(crate) const RECURSION_RETURNED_EXIT_CODE: u32 = 3;
pub(crate) const JOIN_FAILED_EXIT_CODE: u32 = 4;
pub(crate) const UNKNOWN_FRAME_EXIT_CODE: u32 = 5;

#[repr(C)]
pub(crate) struct StackReport {
    /// Размер буфера кадра рекурсии: [`SMALL_FRAME_BYTES`] или [`LARGE_FRAME_BYTES`]. Пишет
    /// запускающая сторона до старта фикстуры.
    pub frame_bytes: AtomicU64,
    /// Адрес локальной переменной на входе в тело потока.
    pub entry_frame: AtomicU64,
    /// Адрес буфера самого глубокого кадра рекурсии, который поток успел записать.
    pub deepest_frame: AtomicU64,
}
