//! Транспорт virtio-mmio версии 2: проверка окна, сброс, признаки, настройка очереди.

use crate::access::{AccessWidth, Registers};

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
    /// Блочное устройство.
    Block = 2,
    /// Устройство GPU.
    Gpu = 16,
    /// Устройство ввода.
    Input = 18,
}

/// Отказ транспорта.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportError<E> {
    /// Обращение к регистру завершилось отказом.
    Register(E),
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
    pub async fn probe(
        registers: R,
        expected: DeviceType,
    ) -> Result<Self, TransportError<R::Error>> {
        let magic = registers
            .read(register::MAGIC_VALUE)
            .await
            .map_err(TransportError::Register)?;
        if magic != TRANSPORT_MAGIC {
            return Err(TransportError::BadMagic(magic));
        }

        let version = registers
            .read(register::VERSION)
            .await
            .map_err(TransportError::Register)?;
        if version != TRANSPORT_VERSION {
            return Err(TransportError::UnsupportedVersion(version));
        }

        let device_type = registers
            .read(register::DEVICE_ID)
            .await
            .map_err(TransportError::Register)?;
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
    /// Отклонение признаков оставляет устройству статус FAILED. Отказ доступа к регистру
    /// прекращает согласование и возвращает его причину.
    pub async fn negotiate(
        &mut self,
        driver_features: u64,
    ) -> Result<u64, TransportError<R::Error>> {
        self.reset().await?;
        self.set_status(status::ACKNOWLEDGE)
            .await
            .map_err(TransportError::Register)?;
        self.set_status(status::ACKNOWLEDGE | status::DRIVER)
            .await
            .map_err(TransportError::Register)?;

        let offered = self
            .read_features()
            .await
            .map_err(TransportError::Register)?;
        if offered & FEATURE_VERSION_1 == 0 {
            self.fail().await.map_err(TransportError::Register)?;
            return Err(TransportError::NoVersion1);
        }

        let accepted = offered & (FEATURE_VERSION_1 | driver_features);
        self.write_features(accepted)
            .await
            .map_err(TransportError::Register)?;

        self.set_status(status::ACKNOWLEDGE | status::DRIVER | status::FEATURES_OK)
            .await
            .map_err(TransportError::Register)?;

        if self
            .registers
            .read(register::STATUS)
            .await
            .map_err(TransportError::Register)?
            & status::FEATURES_OK
            == 0
        {
            self.fail().await.map_err(TransportError::Register)?;
            return Err(TransportError::FeaturesRejected);
        }

        Ok(accepted)
    }

    /// Выставляет DRIVER_OK после настройки очередей.
    pub async fn start(&mut self) -> Result<(), R::Error> {
        let current = self.registers.read(register::STATUS).await?;
        self.set_status(current | status::DRIVER_OK).await
    }

    /// Сбрасывает устройство. После успеха оно не обращается к очередям и их буферам.
    pub async fn reset(&mut self) -> Result<(), TransportError<R::Error>> {
        self.set_status(0).await.map_err(TransportError::Register)?;
        match self
            .registers
            .read(register::STATUS)
            .await
            .map_err(TransportError::Register)?
        {
            0 => Ok(()),
            _ => Err(TransportError::ResetIncomplete),
        }
    }

    /// Причины прерывания, ожидающие подтверждения.
    pub async fn interrupt_status(&self) -> Result<u32, R::Error> {
        self.registers.read(register::INTERRUPT_STATUS).await
    }

    /// Подтверждает причины прерывания из `causes`.
    pub async fn acknowledge_interrupt(&mut self, causes: u32) -> Result<(), R::Error> {
        self.registers.write(register::INTERRUPT_ACK, causes).await
    }

    /// Читает поле конфигурации устройства одним обращением ширины `width` по смещению от
    /// начала конфигурации.
    pub async fn read_config(&self, offset: usize, width: AccessWidth) -> Result<u32, R::Error> {
        self.registers.read_config(offset, width).await
    }

    /// Записывает младшие байты `value` в поле конфигурации устройства одним обращением
    /// ширины `width` по смещению от начала конфигурации.
    pub async fn write_config(
        &mut self,
        offset: usize,
        width: AccessWidth,
        value: u32,
    ) -> Result<(), R::Error> {
        self.registers.write_config(offset, width, value).await
    }

    pub(crate) async fn select_queue(&mut self, index: u16) -> Result<(), R::Error> {
        self.registers
            .write(register::QUEUE_SELECT, u32::from(index))
            .await
    }

    pub(crate) async fn queue_ready(&self) -> Result<bool, R::Error> {
        self.registers
            .read(register::QUEUE_READY)
            .await
            .map(|ready| ready != 0)
    }

    pub(crate) async fn queue_size_max(&self) -> Result<u32, R::Error> {
        self.registers.read(register::QUEUE_SIZE_MAX).await
    }

    pub(crate) async fn configure_queue(
        &mut self,
        size: u16,
        descriptors: u64,
        driver: u64,
        device: u64,
    ) -> Result<(), R::Error> {
        self.registers
            .write(register::QUEUE_SIZE, u32::from(size))
            .await?;

        self.write_address(register::QUEUE_DESCRIPTORS_LOW, descriptors)
            .await?;
        self.write_address(register::QUEUE_DRIVER_LOW, driver)
            .await?;
        self.write_address(register::QUEUE_DEVICE_LOW, device)
            .await?;

        self.registers.write(register::QUEUE_READY, 1).await
    }

    pub(crate) async fn notify(&mut self, index: u16) -> Result<(), R::Error> {
        self.registers
            .write(register::QUEUE_NOTIFY, u32::from(index))
            .await
    }

    pub(crate) fn barrier(&mut self) {
        self.registers.barrier();
    }

    async fn set_status(&mut self, value: u32) -> Result<(), R::Error> {
        self.registers.write(register::STATUS, value).await
    }

    async fn fail(&mut self) -> Result<(), R::Error> {
        let current = self.registers.read(register::STATUS).await?;
        self.set_status(current | status::FAILED).await
    }

    async fn read_features(&mut self) -> Result<u64, R::Error> {
        self.registers
            .write(register::DEVICE_FEATURES_SELECT, 0)
            .await?;
        let low = self.registers.read(register::DEVICE_FEATURES).await?;
        self.registers
            .write(register::DEVICE_FEATURES_SELECT, 1)
            .await?;

        let high = self.registers.read(register::DEVICE_FEATURES).await?;

        Ok(u64::from(high) << 32 | u64::from(low))
    }

    async fn write_features(&mut self, features: u64) -> Result<(), R::Error> {
        let (low, high) = split(features);
        self.registers
            .write(register::DRIVER_FEATURES_SELECT, 0)
            .await?;
        self.registers.write(register::DRIVER_FEATURES, low).await?;
        self.registers
            .write(register::DRIVER_FEATURES_SELECT, 1)
            .await?;

        self.registers.write(register::DRIVER_FEATURES, high).await
    }

    async fn write_address(&mut self, low_offset: usize, address: u64) -> Result<(), R::Error> {
        let (low, high) = split(address);
        self.registers.write(low_offset, low).await?;
        self.registers.write(low_offset + 4, high).await
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
    use crate::testing::{Event, FakeRegisters, Log, RegisterFailure, complete};

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

            let result = complete(Transport::probe(registers, DeviceType::Gpu));

            assert_eq!(result.err(), Some(expected));
            assert!(log.writes().is_empty());
        }
    }

    #[test]
    fn probe_preserves_read_failure_and_stops_access() {
        let offsets = [
            register::MAGIC_VALUE,
            register::VERSION,
            register::DEVICE_ID,
        ];
        for (position, offset) in offsets.into_iter().enumerate() {
            let log = Log::default();
            let registers = FakeRegisters::gpu(&log).refusing_read(offset);

            let result = complete(Transport::probe(registers, DeviceType::Gpu));

            assert_eq!(
                result.err(),
                Some(TransportError::Register(RegisterFailure::Read(offset)))
            );
            assert_eq!(
                log.events(),
                offsets[..=position]
                    .iter()
                    .copied()
                    .map(Event::Read)
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn negotiate_preserves_write_failure_and_stops_access() {
        let log = Log::default();
        let registers = FakeRegisters::gpu(&log).refusing_write(register::DEVICE_FEATURES_SELECT);
        let mut transport = complete(Transport::probe(registers, DeviceType::Gpu)).unwrap();
        log.clear();

        let result = complete(transport.negotiate(0));

        assert_eq!(
            result,
            Err(TransportError::Register(RegisterFailure::Write(
                register::DEVICE_FEATURES_SELECT
            )))
        );
        assert_eq!(status_writes(&log), [0, 1, 3]);
        assert_eq!(
            log.events().last(),
            Some(&Event::Write(register::DEVICE_FEATURES_SELECT, 0))
        );
    }

    #[test]
    fn reset_does_not_confirm_stop_after_register_failure() {
        for failure in [
            RegisterFailure::Read(register::STATUS),
            RegisterFailure::Write(register::STATUS),
        ] {
            let log = Log::default();
            let registers = match failure {
                RegisterFailure::Read(offset) => FakeRegisters::gpu(&log).refusing_read(offset),
                RegisterFailure::Write(offset) => FakeRegisters::gpu(&log).refusing_write(offset),
            };

            let mut transport = complete(Transport::probe(registers, DeviceType::Gpu)).unwrap();
            log.clear();

            let result = complete(transport.reset());

            assert_eq!(result, Err(TransportError::Register(failure)));
            let expected = match failure {
                RegisterFailure::Read(_) => vec![
                    Event::Write(register::STATUS, 0),
                    Event::Read(register::STATUS),
                ],
                RegisterFailure::Write(_) => vec![Event::Write(register::STATUS, 0)],
            };
            assert_eq!(log.events(), expected);
        }
    }

    #[test]
    fn negotiate_and_start_follow_status_sequence() {
        let log = Log::default();
        let registers = FakeRegisters::gpu(&log).with_features(FEATURE_VERSION_1 | 0b110);
        let mut transport = complete(Transport::probe(registers, DeviceType::Gpu)).unwrap();

        let accepted = complete(transport.negotiate(0b011)).unwrap();
        complete(transport.start()).unwrap();

        assert_eq!(accepted, FEATURE_VERSION_1 | 0b010);
        assert_eq!(log.writes_to(register::DRIVER_FEATURES), [0b010, 1]);
        assert_eq!(status_writes(&log), [0, 1, 3, 11, 15]);
    }

    #[test]
    fn negotiate_without_version_1_sets_failed() {
        let log = Log::default();
        let registers = FakeRegisters::gpu(&log).with_features(0b1);
        let mut transport = complete(Transport::probe(registers, DeviceType::Gpu)).unwrap();

        let result = complete(transport.negotiate(0));

        assert_eq!(result, Err(TransportError::NoVersion1));
        assert_eq!(status_writes(&log).last(), Some(&(3 | status::FAILED)));
    }

    #[test]
    fn negotiate_rejected_features_ok_sets_failed() {
        let log = Log::default();
        let registers = FakeRegisters::gpu(&log).rejecting_features();
        let mut transport = complete(Transport::probe(registers, DeviceType::Gpu)).unwrap();

        let result = complete(transport.negotiate(0));

        assert_eq!(result, Err(TransportError::FeaturesRejected));
        assert_eq!(status_writes(&log).last(), Some(&(3 | status::FAILED)));
    }

    #[test]
    fn negotiate_stops_when_reset_is_incomplete() {
        let log = Log::default();
        let registers = FakeRegisters::gpu(&log).holding_reset();
        let mut transport = complete(Transport::probe(registers, DeviceType::Gpu)).unwrap();

        let result = complete(transport.negotiate(0));

        assert_eq!(result, Err(TransportError::ResetIncomplete));
        assert_eq!(status_writes(&log), [0]);
    }
}
