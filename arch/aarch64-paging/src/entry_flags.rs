/// Флаги записи таблицы страниц для aarch64
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryFlags(u64);

impl EntryFlags {
    // Базовые флаги
    pub const VALID: Self = EntryFlags(1 << 0);
    pub const TABLE: Self = EntryFlags(1 << 1);
    pub const PAGE: Self = EntryFlags(1 << 1);
    pub const BLOCK: Self = EntryFlags(0 << 1);
    pub const ACCESS: Self = EntryFlags(1 << 10);

    // Атрибуты памяти (индекс MAIR)
    pub const NORMAL_MEMORY: Self = EntryFlags(0 << 2);
    pub const DEVICE_MEMORY: Self = EntryFlags(1 << 2);

    // Разделяемость
    pub const NON_SHAREABLE: Self = EntryFlags(0 << 8);
    pub const OUTER_SHAREABLE: Self = EntryFlags(2 << 8);
    pub const INNER_SHAREABLE: Self = EntryFlags(3 << 8);

    // Права доступа (биты AP)
    pub const KERNEL_RW: Self = EntryFlags(0 << 6);
    pub const KERNEL_RO: Self = EntryFlags(2 << 6);
    pub const USER_RW: Self = EntryFlags(1 << 6);
    pub const USER_RO: Self = EntryFlags(3 << 6);

    // Права на выполнение
    pub const EXECUTE_NEVER: Self = EntryFlags(1 << 54);
    pub const PRIVILEGED_EXECUTE_NEVER: Self = EntryFlags(1 << 53);

    // Распространенные комбинации
    pub const KERNEL_CODE: Self =
        Self::combine(&[Self::NORMAL_MEMORY, Self::INNER_SHAREABLE, Self::KERNEL_RO]);

    pub const KERNEL_DATA: Self = Self::combine(&[
        Self::NORMAL_MEMORY,
        Self::INNER_SHAREABLE,
        Self::KERNEL_RW,
        Self::EXECUTE_NEVER,
    ]);

    pub const KERNEL_RODATA: Self = Self::combine(&[
        EntryFlags::NORMAL_MEMORY,
        EntryFlags::INNER_SHAREABLE,
        EntryFlags::KERNEL_RO,
        EntryFlags::EXECUTE_NEVER,
    ]);

    pub const USER_CODE: Self = Self::combine(&[
        Self::NORMAL_MEMORY,
        Self::INNER_SHAREABLE,
        Self::USER_RO,
        Self::PRIVILEGED_EXECUTE_NEVER,
    ]);

    pub const USER_DATA: Self = Self::combine(&[
        Self::NORMAL_MEMORY,
        Self::INNER_SHAREABLE,
        Self::USER_RW,
        Self::EXECUTE_NEVER,
        Self::PRIVILEGED_EXECUTE_NEVER,
    ]);

    pub const DEVICE: Self = Self::combine(&[
        Self::DEVICE_MEMORY,
        Self::OUTER_SHAREABLE,
        Self::KERNEL_RW,
        Self::EXECUTE_NEVER,
        Self::PRIVILEGED_EXECUTE_NEVER,
    ]);

    /// Объединить несколько флагов
    pub const fn combine(flags: &[Self]) -> Self {
        let mut result = 0;
        let mut i = 0;
        while i < flags.len() {
            result |= flags[i].0;
            i += 1;
        }
        EntryFlags(result)
    }

    /// Проверить, установлен ли флаг
    pub fn contains(&self, flag: Self) -> bool {
        (self.0 & flag.0) == flag.0
    }

    /// Добавить флаг
    pub fn set(&self, flag: Self) -> Self {
        Self(self.0 | flag.0)
    }

    /// Получить сырое значение
    pub const fn bits(&self) -> u64 {
        self.0
    }
}
