//! Плитки регистров TLMM и таблица выводов SoC.

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
