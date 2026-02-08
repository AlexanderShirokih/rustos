//! Расширения для виртуальных адресов.

use crate::level::Level;
use memory::virtual_address::AlignedVirtualAddress;

/// Извлечение индекса таблицы страниц из виртуального адреса.
pub trait VirtualAddressExt {
    /// Возвращает индекс в таблице уровня `L` (0..511).
    fn index<L: Level>(self) -> usize;
}

impl<const SHIFT: u8> VirtualAddressExt for AlignedVirtualAddress<SHIFT> {
    fn index<L: Level>(self) -> usize {
        (self.as_usize() >> L::SHIFT) & 0x1FF
    }
}
