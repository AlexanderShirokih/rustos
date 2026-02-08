//! Уровни таблицы страниц AArch64.

use memory::physical_address::AlignedPhysicalAddress;

/// Уровень таблицы страниц.
pub trait Level {
    /// Сдвиг для извлечения индекса из виртуального адреса.
    const SHIFT: u8;
}

/// Уровень 0 (512 ГБ на запись).
pub enum L0 {}
/// Уровень 1 (1 ГБ на запись).
pub enum L1 {}
/// Уровень 2 (2 МБ на запись).
pub enum L2 {}
/// Уровень 3 (4 КБ на запись).
pub enum L3 {}

impl Level for L0 {
    const SHIFT: u8 = 39;
}
impl Level for L1 {
    const SHIFT: u8 = 30;
}
impl Level for L2 {
    const SHIFT: u8 = 21;
}
impl Level for L3 {
    const SHIFT: u8 = 12;
}

/// Физический адрес страницы (4 КБ).
pub type PagePa = AlignedPhysicalAddress<{ L3::SHIFT }>;
/// Физический адрес блока L0 (512 ГБ, не используется).
pub type L0BlockPa = AlignedPhysicalAddress<{ L0::SHIFT }>;
/// Физический адрес блока L1 (1 ГБ).
pub type L1BlockPa = AlignedPhysicalAddress<{ L1::SHIFT }>;
/// Физический адрес блока L2 (2 МБ).
pub type L2BlockPa = AlignedPhysicalAddress<{ L2::SHIFT }>;
