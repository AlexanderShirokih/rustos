//! Контроллер по описанию: блок `reg` и число выводов из таблицы совместимостей.

use platform_abi::{Description, DeviceId};

pub(crate) const COMPATIBLE: &str = "qcom,spmi-gpio";

const PIN_COUNTS: [(&str, u8); 2] = [("qcom,pm660-gpio", 13), ("qcom,pm660l-gpio", 12)];

const BLOCK_BYTES: u32 = 0x100;

/// Контроллер GPIO PMIC: путь узла, блок его `reg` и число выводов.
#[derive(Clone, PartialEq, Eq)]
pub struct ControllerNode {
    /// Путь узла контроллера.
    pub id: DeviceId,
    base: u8,
    pins: u8,
}

impl ControllerNode {
    /// Контроллер узла `id` либо `None`, если узел не совместим с `qcom,spmi-gpio`, ни одна его
    /// строка `compatible` не входит в таблицу числа выводов либо первая ячейка `reg` не задаёт
    /// начало блока.
    pub fn parse(description: &Description<'_>, id: &DeviceId) -> Option<Self> {
        let node = description.find(id.as_str())?;
        let compatible: alloc::vec::Vec<&str> = node.strings("compatible").ok()??.collect();
        if !compatible.contains(&COMPATIBLE) {
            return None;
        }

        let pins = PIN_COUNTS
            .iter()
            .find_map(|(name, pins)| compatible.contains(name).then_some(*pins))?;
        let base = node
            .cells("reg")
            .ok()??
            .next()
            .filter(|address| address.is_multiple_of(BLOCK_BYTES))
            .and_then(|address| u8::try_from(address / BLOCK_BYTES).ok())?;

        Some(Self {
            id: id.clone(),
            base,
            pins,
        })
    }

    /// Число выводов контроллера.
    pub fn pins(&self) -> u8 {
        self.pins
    }

    /// Блок вывода `pin` с номером от 1 либо `None` для номера вне контроллера.
    pub fn block(&self, pin: u16) -> Option<u8> {
        let offset = u8::try_from(pin.checked_sub(1)?).ok()?;
        if offset >= self.pins {
            return None;
        }
        self.base.checked_add(offset)
    }
}
