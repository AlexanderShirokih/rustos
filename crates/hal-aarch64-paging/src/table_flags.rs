//! Иерархические атрибуты записей-таблиц.

/// Флаги записи-таблицы.
///
/// Влияют на все дочерние записи (PXNTable, UXNTable).
#[repr(transparent)]
#[derive(Copy, Clone)]
pub struct TableFlags(u64);

impl TableFlags {
    pub const fn new() -> Self {
        Self(0)
    }

    /// Устанавливает PXNTable - запрет исполнения для EL1.
    pub const fn pxn_table(self, on: bool) -> Self {
        if on { Self(self.0 | (1 << 59)) } else { self }
    }

    /// Устанавливает UXNTable - запрет исполнения для EL0.
    pub const fn uxn_table(self, on: bool) -> Self {
        if on { Self(self.0 | (1 << 60)) } else { self }
    }

    pub const fn bits(self) -> u64 {
        self.0
    }
}

impl Default for TableFlags {
    fn default() -> Self {
        Self::new()
    }
}
