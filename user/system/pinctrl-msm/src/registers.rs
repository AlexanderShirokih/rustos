//! Доступ к 32-битным регистрам плиток TLMM и таблица выводов SoC.

use platform_abi::Node;

const WINDOW_NAMES: &str = "reg-names";

/// Плитка TLMM: окно регистров по имени записи в `reg-names` узла контроллера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tile {
    /// Окно `north`.
    North,
    /// Окно `center`.
    Center,
    /// Окно `south`.
    South,
}

/// Вывод GPIO из таблицы SoC: плитка его регистров и функции сверх `gpio` в порядке их
/// номеров с 1. Пустое имя - позиция без функции.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinGroup {
    /// Плитка регистров вывода.
    pub tile: Tile,
    /// Функции вывода с номерами 1..=9.
    pub functions: [&'static str; 9],
}

/// Регистры плиток контроллера. Реализация задаёт размер окна плитки и доступ к его 32-битным
/// регистрам. Смещение в [`load`](Registers::load) и [`store`](Registers::store) выровнено по
/// регистру и лежит внутри окна.
pub trait Registers {
    /// Размер окна плитки `tile` в байтах.
    fn size(&self, tile: Tile) -> usize;

    /// Читает регистр плитки `tile` по смещению `offset`.
    fn load(&self, tile: Tile, offset: usize) -> u32;

    /// Записывает регистр плитки `tile` по смещению `offset`.
    fn store(&self, tile: Tile, offset: usize, value: u32);
}

impl Tile {
    const fn name(self) -> &'static str {
        match self {
            Self::North => "north",
            Self::Center => "center",
            Self::South => "south",
        }
    }

    /// Номер окна плитки в `reg` узла контроллера `node`: позиция её имени в `reg-names`.
    /// Узел без этого имени окна плитки не имеет.
    pub fn resource(self, node: &Node<'_>) -> Option<u16> {
        let position = node
            .strings(WINDOW_NAMES)
            .ok()??
            .position(|name| name == self.name())?;
        u16::try_from(position).ok()
    }
}
