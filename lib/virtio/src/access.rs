//! Доступ библиотеки к регистрам транспорта и к памяти, общей с устройством.

/// Доступ к регистрам транспорта virtio-mmio.
pub trait Registers {
    /// Тип отказа обращения.
    type Error;

    /// Читает 32-битный регистр по смещению от начала окна.
    fn read(&self, offset: usize) -> Result<u32, Self::Error>;

    /// Записывает 32-битный регистр по смещению от начала окна.
    fn write(&mut self, offset: usize, value: u32) -> Result<(), Self::Error>;

    /// Завершает для устройства обращения к памяти и регистрам до барьера раньше обращений
    /// после него.
    fn barrier(&mut self);
}

/// Обращение выходит за границу региона.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutOfRange;

/// Непрерывный регион памяти, доступный устройству по физическому адресу.
///
/// Чтение и запись двух байт по чётному смещению выполняются одним обращением к памяти.
pub trait DeviceMemory {
    /// Копирует байты региона с `offset` в `destination`.
    fn read(&self, offset: usize, destination: &mut [u8]) -> Result<(), OutOfRange>;

    /// Копирует `source` в регион с `offset`.
    fn write(&mut self, offset: usize, source: &[u8]) -> Result<(), OutOfRange>;

    /// Физический адрес начала региона.
    fn physical_address(&self) -> u64;
}
