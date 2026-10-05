//! Транспорт virtio-mmio версии 2: проверка окна, сброс, признаки, настройка очереди.

use crate::access::Registers;

const TRANSPORT_MAGIC: u32 = 0x7472_6976;
const TRANSPORT_VERSION: u32 = 2;
const FEATURE_VERSION_1: u64 = 1 << 32;

/// Смещения регистров окна транспорта.
pub mod register {
    pub(crate) const MAGIC_VALUE: usize = 0x000;
    pub(crate) const VERSION: usize = 0x004;
    /// Тип устройства, подключённого к транспорту. У пустого транспорта 0.
    pub const DEVICE_ID: usize = 0x008;
    pub(crate) const DEVICE_FEATURES: usize = 0x010;
    pub(crate) const DEVICE_FEATURES_SELECT: usize = 0x014;
    pub(crate) const DRIVER_FEATURES: usize = 0x020;
    pub(crate) const DRIVER_FEATURES_SELECT: usize = 0x024;
    pub(crate) const QUEUE_SELECT: usize = 0x030;
    pub(crate) const QUEUE_SIZE_MAX: usize = 0x034;
    pub(crate) const QUEUE_SIZE: usize = 0x038;
    pub(crate) const QUEUE_READY: usize = 0x044;
    pub(crate) const QUEUE_NOTIFY: usize = 0x050;
    pub(crate) const INTERRUPT_STATUS: usize = 0x060;
    pub(crate) const INTERRUPT_ACK: usize = 0x064;
    /// Статус устройства. Запись 0 сбрасывает устройство.
    pub const STATUS: usize = 0x070;
    pub(crate) const QUEUE_DESCRIPTORS_LOW: usize = 0x080;
    pub(crate) const QUEUE_DRIVER_LOW: usize = 0x090;
    pub(crate) const QUEUE_DEVICE_LOW: usize = 0x0a0;
}

pub(crate) mod status {
    pub(crate) const ACKNOWLEDGE: u32 = 1;
    pub(crate) const DRIVER: u32 = 2;
    pub(crate) const DRIVER_OK: u32 = 4;
    pub(crate) const FEATURES_OK: u32 = 8;
    pub(crate) const FAILED: u32 = 128;
}

/// Класс устройства, который драйвер ожидает за транспортом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum DeviceType {
    /// Устройство GPU.
    Gpu = 16,
}

/// Отказ транспорта.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportError {
    /// Регистр магического числа не содержит `virt`.
    BadMagic(u32),
    /// Версия транспорта отличается от 2, версия 1 - устаревший транспорт.
    UnsupportedVersion(u32),
    /// За транспортом нет устройства: тип 0.
    NoDevice,
    /// Тип устройства отличается от запрошенного драйвером.
    WrongDeviceType(u32),
    /// Устройство не предлагает признак версии 1.0.
    NoVersion1,
    /// Устройство не подтвердило FEATURES_OK.
    FeaturesRejected,
    /// Статус после записи нуля прочитан ненулевым. Сброс не завершён, вызов повторяется.
    ResetIncomplete,
}

/// Проверенное окно транспорта версии 2 с устройством ожидаемого класса.
pub struct Transport<R> {
    registers: R,
}

impl<R: Registers> Transport<R> {
    /// Проверяет магическое число, версию и тип устройства, ни один регистр не записывая.
    pub fn probe(registers: R, expected: DeviceType) -> Result<Self, TransportError> {
        let magic = registers.read(register::MAGIC_VALUE);
        if magic != TRANSPORT_MAGIC {
            return Err(TransportError::BadMagic(magic));
        }

        let version = registers.read(register::VERSION);
        if version != TRANSPORT_VERSION {
            return Err(TransportError::UnsupportedVersion(version));
        }

        let device_type = registers.read(register::DEVICE_ID);
        if device_type == 0 {
            return Err(TransportError::NoDevice);
        }
        if device_type != expected as u32 {
            return Err(TransportError::WrongDeviceType(device_type));
        }

        Ok(Self { registers })
    }

    /// Сбрасывает устройство, принимает из предложенных признаков версию 1.0 и
    /// `driver_features` и возвращает принятые признаки.
    ///
    /// Отказ оставляет устройству статус FAILED.
    pub fn negotiate(&mut self, driver_features: u64) -> Result<u64, TransportError> {
        self.reset()?;
        self.set_status(status::ACKNOWLEDGE);
        self.set_status(status::ACKNOWLEDGE | status::DRIVER);

        let offered = self.read_features();
        if offered & FEATURE_VERSION_1 == 0 {
            self.fail();
            return Err(TransportError::NoVersion1);
        }

        let accepted = offered & (FEATURE_VERSION_1 | driver_features);
        self.write_features(accepted);
        self.set_status(status::ACKNOWLEDGE | status::DRIVER | status::FEATURES_OK);
        if self.registers.read(register::STATUS) & status::FEATURES_OK == 0 {
            self.fail();
            return Err(TransportError::FeaturesRejected);
        }

        Ok(accepted)
    }

    /// Выставляет DRIVER_OK после настройки очередей.
    pub fn start(&mut self) {
        let current = self.registers.read(register::STATUS);
        self.set_status(current | status::DRIVER_OK);
    }

    /// Сбрасывает устройство. После успеха оно не обращается к очередям.
    pub fn reset(&mut self) -> Result<(), TransportError> {
        self.set_status(0);
        match self.registers.read(register::STATUS) {
            0 => Ok(()),
            _ => Err(TransportError::ResetIncomplete),
        }
    }

    /// Причины прерывания, ожидающие подтверждения.
    pub fn interrupt_status(&self) -> u32 {
        self.registers.read(register::INTERRUPT_STATUS)
    }

    /// Подтверждает причины прерывания из `causes`.
    pub fn acknowledge_interrupt(&mut self, causes: u32) {
        self.registers.write(register::INTERRUPT_ACK, causes);
    }

    pub(crate) fn select_queue(&mut self, index: u16) {
        self.registers
            .write(register::QUEUE_SELECT, u32::from(index));
    }

    pub(crate) fn queue_ready(&self) -> bool {
        self.registers.read(register::QUEUE_READY) != 0
    }

    pub(crate) fn queue_size_max(&self) -> u32 {
        self.registers.read(register::QUEUE_SIZE_MAX)
    }

    pub(crate) fn configure_queue(
        &mut self,
        size: u16,
        descriptors: u64,
        driver: u64,
        device: u64,
    ) {
        self.registers.write(register::QUEUE_SIZE, u32::from(size));
        self.write_address(register::QUEUE_DESCRIPTORS_LOW, descriptors);
        self.write_address(register::QUEUE_DRIVER_LOW, driver);
        self.write_address(register::QUEUE_DEVICE_LOW, device);
        self.registers.write(register::QUEUE_READY, 1);
    }

    pub(crate) fn notify(&mut self, index: u16) {
        self.registers
            .write(register::QUEUE_NOTIFY, u32::from(index));
    }

    pub(crate) fn barrier(&mut self) {
        self.registers.barrier();
    }

    fn set_status(&mut self, value: u32) {
        self.registers.write(register::STATUS, value);
    }

    fn fail(&mut self) {
        let current = self.registers.read(register::STATUS);
        self.set_status(current | status::FAILED);
    }

    fn read_features(&mut self) -> u64 {
        self.registers.write(register::DEVICE_FEATURES_SELECT, 0);
        let low = self.registers.read(register::DEVICE_FEATURES);
        self.registers.write(register::DEVICE_FEATURES_SELECT, 1);
        let high = self.registers.read(register::DEVICE_FEATURES);
        u64::from(high) << 32 | u64::from(low)
    }

    fn write_features(&mut self, features: u64) {
        let (low, high) = split(features);
        self.registers.write(register::DRIVER_FEATURES_SELECT, 0);
        self.registers.write(register::DRIVER_FEATURES, low);
        self.registers.write(register::DRIVER_FEATURES_SELECT, 1);
        self.registers.write(register::DRIVER_FEATURES, high);
    }

    fn write_address(&mut self, low_offset: usize, address: u64) {
        let (low, high) = split(address);
        self.registers.write(low_offset, low);
        self.registers.write(low_offset + 4, high);
    }
}

fn split(value: u64) -> (u32, u32) {
    let [l0, l1, l2, l3, h0, h1, h2, h3] = value.to_le_bytes();
    (
        u32::from_le_bytes([l0, l1, l2, l3]),
        u32::from_le_bytes([h0, h1, h2, h3]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{FakeRegisters, Log};

    fn status_writes(log: &Log) -> Vec<u32> {
        log.writes_to(register::STATUS)
    }

    #[test]
    fn probe_rejects_invalid_window_without_writes() {
        let cases = [
            (
                register::MAGIC_VALUE,
                0x1234,
                TransportError::BadMagic(0x1234),
            ),
            (register::VERSION, 1, TransportError::UnsupportedVersion(1)),
            (register::DEVICE_ID, 0, TransportError::NoDevice),
            (register::DEVICE_ID, 2, TransportError::WrongDeviceType(2)),
        ];

        for (offset, value, expected) in cases {
            let log = Log::default();
            let mut registers = FakeRegisters::gpu(&log);
            registers.set(offset, value);

            let result = Transport::probe(registers, DeviceType::Gpu);

            assert_eq!(result.err(), Some(expected));
            assert!(log.writes().is_empty());
        }
    }

    #[test]
    fn negotiate_and_start_follow_status_sequence() {
        let log = Log::default();
        let registers = FakeRegisters::gpu(&log).with_features(FEATURE_VERSION_1 | 0b110);
        let mut transport = Transport::probe(registers, DeviceType::Gpu).unwrap();

        let accepted = transport.negotiate(0b011).unwrap();
        transport.start();

        assert_eq!(accepted, FEATURE_VERSION_1 | 0b010);
        assert_eq!(log.writes_to(register::DRIVER_FEATURES), [0b010, 1]);
        assert_eq!(status_writes(&log), [0, 1, 3, 11, 15]);
    }

    #[test]
    fn negotiate_without_version_1_sets_failed() {
        let log = Log::default();
        let registers = FakeRegisters::gpu(&log).with_features(0b1);
        let mut transport = Transport::probe(registers, DeviceType::Gpu).unwrap();

        let result = transport.negotiate(0);

        assert_eq!(result, Err(TransportError::NoVersion1));
        assert_eq!(status_writes(&log).last(), Some(&(3 | status::FAILED)));
    }

    #[test]
    fn negotiate_rejected_features_ok_sets_failed() {
        let log = Log::default();
        let registers = FakeRegisters::gpu(&log).rejecting_features();
        let mut transport = Transport::probe(registers, DeviceType::Gpu).unwrap();

        let result = transport.negotiate(0);

        assert_eq!(result, Err(TransportError::FeaturesRejected));
        assert_eq!(status_writes(&log).last(), Some(&(3 | status::FAILED)));
    }

    #[test]
    fn negotiate_stops_when_reset_is_incomplete() {
        let log = Log::default();
        let registers = FakeRegisters::gpu(&log).holding_reset();
        let mut transport = Transport::probe(registers, DeviceType::Gpu).unwrap();

        let result = transport.negotiate(0);

        assert_eq!(result, Err(TransportError::ResetIncomplete));
        assert_eq!(status_writes(&log), [0]);
    }
}
