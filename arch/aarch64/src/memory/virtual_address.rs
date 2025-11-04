/// Адрес виртуальной памяти
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct VirtualAddress(pub usize);

impl VirtualAddress {
    /// Создать новый виртуальный адрес
    pub const fn new(address: usize) -> Self {
        VirtualAddress(address)
    }

    /// Получить сырое значение адреса
    pub const fn as_usize(&self) -> usize {
        self.0
    }
}

pub(crate) trait VirtualAddressExt {
    /// Получить смещение страницы (биты 0-11)
    fn page_offset(&self) -> usize;

    /// Получить индексы таблицы страниц для этого адреса (4-уровневая подкачка aarch64)
    fn page_table_indices(&self) -> [usize; 4];
}

impl VirtualAddressExt for VirtualAddress {
    fn page_offset(&self) -> usize {
        self.0 & 0xFFF
    }

    fn page_table_indices(&self) -> [usize; 4] {
        let addr = self.0;
        [
            (addr >> 39) & 0x1FF, // Уровень 0 (PGD)
            (addr >> 30) & 0x1FF, // Уровень 1 (PUD)
            (addr >> 21) & 0x1FF, // Уровень 2 (PMD)
            (addr >> 12) & 0x1FF, // Уровень 3 (PTE)
        ]
    }
}
