use alloc::{collections::BTreeMap, vec::Vec};

use platform_abi::{DeviceClaims, DeviceId};

/// Вывод контроллера выводов.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pin {
    /// Путь узла контроллера.
    pub controller: DeviceId,
    /// Номер вывода в контроллере.
    pub number: u16,
}

/// Узлы и выводы, занятые сессиями поставщика выводов, по значкам сессий.
#[derive(Default)]
pub struct Claims {
    devices: DeviceClaims,
    pins: BTreeMap<Pin, u64>,
}

impl Claims {
    /// Таблица без занятых узлов.
    pub const fn new() -> Self {
        Self {
            devices: DeviceClaims::new(),
            pins: BTreeMap::new(),
        }
    }

    /// Возвращает `true`, если узел `device` занят какой-либо сессией.
    pub fn holds_device(&self, device: &DeviceId) -> bool {
        self.devices.holds_device(device)
    }

    /// Возвращает `true`, если какая-либо сессия занимает хотя бы один вывод из `pins`.
    pub fn holds_any(&self, pins: &[Pin]) -> bool {
        pins.iter().any(|pin| self.pins.contains_key(pin))
    }

    /// Возвращает `true`, если сессия со значком `badge` занимает узел.
    pub fn holds_session(&self, badge: u64) -> bool {
        self.devices.holds_session(badge)
    }

    /// Занимает узел `device` и выводы `pins` сессией со значком `badge`.
    pub fn claim(&mut self, badge: u64, device: DeviceId, pins: Vec<Pin>) {
        self.devices.claim(badge, device);
        self.pins.extend(pins.into_iter().map(|pin| (pin, badge)));
    }

    /// Значок сессии, которая занимает вывод `pin`.
    pub fn holder(&self, pin: &Pin) -> Option<u64> {
        self.pins.get(pin).copied()
    }

    /// Занимает вывод `pin` сессией со значком `badge`.
    pub fn claim_pin(&mut self, badge: u64, pin: Pin) {
        self.pins.insert(pin, badge);
    }

    /// Освобождает узел и выводы сессии со значком `badge`.
    pub fn release(&mut self, badge: u64) {
        self.devices.release(badge);
        self.pins.retain(|_, holder| *holder != badge);
    }
}
