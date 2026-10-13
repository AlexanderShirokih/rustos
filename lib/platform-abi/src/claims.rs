use alloc::collections::BTreeMap;

use crate::device::DeviceId;

/// Узлы, занятые сессиями поставщика ресурсов узлов, со значками сессий. Сессия держит любое
/// число узлов.
#[derive(Default)]
pub struct DeviceClaims {
    holders: BTreeMap<DeviceId, u64>,
}

impl DeviceClaims {
    /// Таблица без занятых узлов.
    pub const fn new() -> Self {
        Self {
            holders: BTreeMap::new(),
        }
    }

    /// Значок сессии, которая занимает узел `device`.
    pub fn holder(&self, device: &DeviceId) -> Option<u64> {
        self.holders.get(device).copied()
    }

    /// Возвращает `true`, если узел `device` занят какой-либо сессией.
    pub fn holds_device(&self, device: &DeviceId) -> bool {
        self.holders.contains_key(device)
    }

    /// Возвращает `true`, если сессия со значком `badge` занимает хотя бы один узел.
    pub fn holds_session(&self, badge: u64) -> bool {
        self.holders.values().any(|holder| *holder == badge)
    }

    /// Узлы, которые занимает сессия со значком `badge`.
    pub fn devices(&self, badge: u64) -> impl Iterator<Item = &DeviceId> + '_ {
        self.holders
            .iter()
            .filter(move |(_, holder)| **holder == badge)
            .map(|(device, _)| device)
    }

    /// Занимает узел `device` сессией со значком `badge`.
    pub fn claim(&mut self, badge: u64, device: DeviceId) {
        self.holders.insert(device, badge);
    }

    /// Освобождает узлы сессии со значком `badge`.
    pub fn release(&mut self, badge: u64) {
        self.holders.retain(|_, holder| *holder != badge);
    }
}
