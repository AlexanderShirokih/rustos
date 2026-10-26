//! Идентичность клавиш и осей.

pub(crate) const USAGE_SIZE: usize = 4;

/// Usage из HID Usage Tables: страница и номер на ней.
///
/// Клавиши клавиатуры и кнопки устройств делят одно пространство usage. Usage без имени
/// сохраняет страницу и номер.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Usage {
    pub page: u16,
    pub id: u16,
}

impl Usage {
    /// Горизонтальная ось.
    pub const X: Self = Self::new(0x01, 0x30);
    /// Вертикальная ось.
    pub const Y: Self = Self::new(0x01, 0x31);

    pub const ENTER: Self = Self::new(0x07, 0x28);
    pub const ESCAPE: Self = Self::new(0x07, 0x29);
    pub const BACKSPACE: Self = Self::new(0x07, 0x2A);
    pub const TAB: Self = Self::new(0x07, 0x2B);
    pub const F1: Self = Self::new(0x07, 0x3A);

    pub const RIGHT: Self = Self::new(0x07, 0x4F);
    pub const LEFT: Self = Self::new(0x07, 0x50);
    pub const DOWN: Self = Self::new(0x07, 0x51);
    pub const UP: Self = Self::new(0x07, 0x52);
    pub const HOME: Self = Self::new(0x07, 0x4A);

    pub const POWER: Self = Self::new(0x07, 0x66);
    pub const VOLUME_UP: Self = Self::new(0x07, 0x80);
    pub const VOLUME_DOWN: Self = Self::new(0x07, 0x81);

    /// Основная кнопка указателя.
    pub const PRIMARY: Self = Self::new(0x09, 0x01);
    /// Дополнительная кнопка указателя.
    pub const SECONDARY: Self = Self::new(0x09, 0x02);
    /// Средняя кнопка указателя.
    pub const MIDDLE: Self = Self::new(0x09, 0x03);

    pub const fn new(page: u16, id: u16) -> Self {
        Self { page, id }
    }

    pub(crate) const fn to_bytes(self) -> [u8; USAGE_SIZE] {
        let [p0, p1] = self.page.to_le_bytes();
        let [i0, i1] = self.id.to_le_bytes();
        [p0, p1, i0, i1]
    }

    pub(crate) const fn from_bytes(bytes: [u8; USAGE_SIZE]) -> Self {
        let [p0, p1, i0, i1] = bytes;
        Self::new(u16::from_le_bytes([p0, p1]), u16::from_le_bytes([i0, i1]))
    }
}
