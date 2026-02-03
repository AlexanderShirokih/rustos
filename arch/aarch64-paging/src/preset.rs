//! Готовые пресеты атрибутов памяти.

use crate::mem_flags::{Access, MemFlags, Shareability};

/// Исполняемый код ядра.
pub struct KernelText;

/// Данные ядра (чтение/запись).
pub struct KernelData;

/// Константные данные ядра.
pub struct KernelRoData;

/// Регистры устройств (MMIO).
pub struct Mmio;

/// Динамическая память ядра.
pub struct Heap;

impl KernelText {
    /// Возвращает флаги для исполняемого кода ядра.
    pub const fn flags() -> MemFlags {
        MemFlags::new()
            .af(true)
            .sh(Shareability::Inner)
            .ap(Access::KernelRW)
            .attr_index(0)
            .pxn(false)
            .uxn(true)
    }
}

impl KernelData {
    /// Возвращает флаги для данных ядра.
    pub const fn flags() -> MemFlags {
        MemFlags::new()
            .af(true)
            .sh(Shareability::Inner)
            .ap(Access::KernelRW)
            .attr_index(0)
            .pxn(true)
            .uxn(true)
    }
}

impl KernelRoData {
    /// Возвращает флаги для константных данных ядра.
    pub const fn flags() -> MemFlags {
        MemFlags::new()
            .af(true)
            .sh(Shareability::Inner)
            .ap(Access::KernelRO)
            .attr_index(0)
            .pxn(false)
            .uxn(true)
    }
}

impl Heap {
    /// Возвращает флаги для кучи ядра.
    pub const fn flags() -> MemFlags {
        MemFlags::new()
            .af(true)
            .sh(Shareability::Inner)
            .ap(Access::KernelRW)
            .attr_index(0)
            .pxn(true)
            .uxn(true)
    }
}

impl Mmio {
    /// Возвращает флаги для MMIO-регионов.
    pub const fn flags() -> MemFlags {
        MemFlags::new()
            .af(true)
            .sh(Shareability::None)
            .ap(Access::KernelRW)
            .attr_index(1)
            .pxn(true)
            .uxn(true)
    }
}
