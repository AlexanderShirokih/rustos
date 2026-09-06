//! Готовые пресеты атрибутов памяти.

use crate::{
    mem_flags::{Aarch64MemFlags, Access, Shareability},
    memory_attribute::MemoryAttribute,
};

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
    /// EL1 читает и пишет. EL0 доступа не получает. Исполнение разрешено из EL1 и
    /// запрещено из EL0.
    pub const fn flags() -> Aarch64MemFlags {
        Aarch64MemFlags::new()
            .af(true)
            .sh(Shareability::Inner)
            .ap(Access::KernelRW)
            .attr_index(MemoryAttribute::Cached.index())
            .pxn(false)
            .uxn(true)
    }
}

impl KernelData {
    /// EL1 читает и пишет. EL0 доступа не получает. Исполнение запрещено на обоих уровнях.
    pub const fn flags() -> Aarch64MemFlags {
        Aarch64MemFlags::new()
            .af(true)
            .sh(Shareability::Inner)
            .ap(Access::KernelRW)
            .attr_index(MemoryAttribute::Cached.index())
            .pxn(true)
            .uxn(true)
    }
}

impl KernelRoData {
    /// EL1 читает. EL0 доступа не получает. Исполнение разрешено из EL1 (`pxn` не
    /// установлен) и запрещено из EL0.
    pub const fn flags() -> Aarch64MemFlags {
        Aarch64MemFlags::new()
            .af(true)
            .sh(Shareability::Inner)
            .ap(Access::KernelRO)
            .attr_index(MemoryAttribute::Cached.index())
            .pxn(false)
            .uxn(true)
    }
}

impl Heap {
    /// EL1 читает и пишет. EL0 доступа не получает. Исполнение запрещено на обоих уровнях.
    pub const fn flags() -> Aarch64MemFlags {
        Aarch64MemFlags::new()
            .af(true)
            .sh(Shareability::Inner)
            .ap(Access::KernelRW)
            .attr_index(MemoryAttribute::Cached.index())
            .pxn(true)
            .uxn(true)
    }
}

impl Mmio {
    /// EL1 читает и пишет. EL0 доступа не получает. Исполнение запрещено на обоих
    /// уровнях. Память не кэшируется и не разделяется между наблюдателями.
    pub const fn flags() -> Aarch64MemFlags {
        Aarch64MemFlags::new()
            .af(true)
            .sh(Shareability::None)
            .ap(Access::KernelRW)
            .attr_index(MemoryAttribute::Device.index())
            .pxn(true)
            .uxn(true)
    }
}
