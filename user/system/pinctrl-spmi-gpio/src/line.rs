//! Линия записи `gpios` под контроллером GPIO PMIC.

use pins_abi::{GPIO_ACTIVE_LOW, Pin, PinsError};
use platform_abi::{Description, DeviceId};

use crate::controller::ControllerNode;

/// Линия-вход: вывод контроллера и полярность записи.
#[derive(Clone, PartialEq, Eq)]
pub struct Line {
    /// Контроллер линии.
    pub controller: ControllerNode,
    /// Номер вывода линии от 1.
    pub pin: u16,
    /// Блок вывода линии.
    pub block: u8,
    /// Запись содержит флаг `GPIO_ACTIVE_LOW`.
    pub active_low: bool,
}

impl Line {
    /// Вывод линии в таблице занятости.
    pub fn pin(&self) -> Pin {
        Pin {
            controller: self.controller.id.clone(),
            number: self.pin,
        }
    }

    /// Возвращает `true`, если уровень `high` вывода - активный уровень линии.
    pub fn active(&self, high: bool) -> bool {
        high != self.active_low
    }
}

/// Линия `index` с именем `name` узла `node` для сессии узла `session`. Ячейки
/// записи - номер вывода от 1 и флаги.
///
/// Отказы:
/// - [`PinsError::NoDevice`] - `node` не узел сессии и не его потомок либо в описании нет
///   записи линии;
/// - [`PinsError::Unsupported`] - контроллер записи не GPIO PMIC либо вне таблицы, вывод вне
///   контроллера, число ячеек кроме двух либо флаги кроме `GPIO_ACTIVE_LOW`.
pub fn line(
    description: &Description<'_>,
    session: &DeviceId,
    node: &DeviceId,
    name: &str,
    index: u8,
) -> Result<Line, PinsError> {
    let record =
        pins_abi::line(description, session, node, name, index).ok_or(PinsError::NoDevice)?;
    let controller =
        ControllerNode::parse(description, &record.controller).ok_or(PinsError::Unsupported)?;

    let [pin, flags] = record.cells[..] else {
        return Err(PinsError::Unsupported);
    };
    let pin = u16::try_from(pin).map_err(|_| PinsError::Unsupported)?;
    let block = controller
        .block(pin)
        .filter(|_| flags & !GPIO_ACTIVE_LOW == 0)
        .ok_or(PinsError::Unsupported)?;

    Ok(Line {
        controller,
        pin,
        block,
        active_low: flags & GPIO_ACTIVE_LOW != 0,
    })
}
