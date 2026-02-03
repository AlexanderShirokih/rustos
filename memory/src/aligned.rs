/// Трейт для типов, представляющих адрес памяти.
pub trait Address: Copy + Clone + PartialOrd {
    /// Возвращает адрес как `usize`.
    fn as_usize(self) -> usize;

    /// Возвращает адрес как `u64`.
    fn as_u64(self) -> u64 {
        self.as_usize() as u64
    }
}

/// Трейт для типов с гарантированным выравниванием.
pub trait Aligned {
    /// Размер выравнивания в байтах.
    const ALIGNMENT: usize;
}
