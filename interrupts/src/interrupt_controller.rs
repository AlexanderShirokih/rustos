//! Контракты подсистемы прерываний.

use core::fmt::Display;

/// Номер аппаратного прерывания.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct IrqNumber(u16);

impl IrqNumber {
    pub const fn new(raw: u16) -> Self {
        Self(raw)
    }

    /// Возвращает сырое значение IRQ.
    pub const fn raw(self) -> u16 {
        self.0
    }
}

/// Приоритет прерывания (0 = наивысший, 255 = наинизший).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct IrqPriority(u8);

impl IrqPriority {
    /// Наивысший приоритет.
    pub const HIGHEST: Self = Self(0);
    /// Наинизший приоритет.
    pub const LOWEST: Self = Self(255);

    /// Создаёт приоритет из сырого значения.
    pub const fn new(priority: u8) -> Self {
        Self(priority)
    }

    /// Возвращает сырое значение приоритета.
    pub const fn raw(self) -> u8 {
        self.0
    }
}

/// Тип прерывания в контексте GIC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrqType {
    /// Software Generated Interrupt (0–15).
    Sgi(IrqNumber),
    /// Private Peripheral Interrupt (16–31).
    Ppi(IrqNumber),
    /// Shared Peripheral Interrupt (32–1019).
    Spi(IrqNumber),
}

impl IrqType {
    /// Создаёт тип прерывания из номера.
    pub fn from_irq_number(irq: IrqNumber) -> Option<Self> {
        match irq.raw() {
            0..=15 => Some(IrqType::Sgi(irq)),
            16..=31 => Some(IrqType::Ppi(irq)),
            32..=1019 => Some(IrqType::Spi(irq)),
            _ => None,
        }
    }

    /// Возвращает номер прерывания.
    pub const fn irq(&self) -> IrqNumber {
        match self {
            IrqType::Sgi(irq) | IrqType::Ppi(irq) | IrqType::Spi(irq) => *irq,
        }
    }
}

/// Маска целевых CPU для маршрутизации прерываний.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct CpuMask(u8);

impl CpuMask {
    /// Запрет на все CPU.
    pub const NONE: Self = CpuMask(0);

    /// Все CPU.
    pub const ALL: Self = Self(0xFF);

    /// CPU 0.
    pub const CPU0: Self = Self(0b0001);
    /// CPU 1.
    pub const CPU1: Self = Self(0b0010);
    /// CPU 2.
    pub const CPU2: Self = Self(0b0100);
    /// CPU 3.
    pub const CPU3: Self = Self(0b1000);

    /// Создаёт маску из сырого значения.
    pub fn cpu(cpu_index: u8) -> Option<Self> {
        cpu_index.checked_add(1).map(Self)
    }

    /// Возвращает сырое значение маски.
    pub const fn raw(self) -> u8 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct IrqBound(pub u32);

/// Ошибки регистрации обработчика IRQ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrqRegistrationError {
    /// Некорректный номер IRQ.
    InvalidIrq,
    /// Обработчик для данного IRQ уже существует.
    AlreadyRegistered,
    /// Регистрация пока не поддерживается текущим окружением.
    Unsupported,
    /// Прочая ошибка с сообщением.
    Other(&'static str),
}

/// Контракт обработчика IRQ.
///
/// Трейт object-safe, чтобы драйвер мог передавать stateful обработчик.
pub trait IrqHandler: Sync {
    /// Вызывается при срабатывании соответствующего IRQ.
    fn handle(&self, irq: IrqNumber);
}

pub enum InterruptControllerInitializationError {
    Error(&'static str),
}

impl Display for InterruptControllerInitializationError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            InterruptControllerInitializationError::Error(err) => f.write_str(err),
        }
    }
}

/// Контракт контроллера прерываний (напр., GIC).
///
/// Трейт определяет базовые операции для управления аппаратным контроллером прерываний.
pub trait InterruptController: Send + Sync {
    /// Инициализирует контроллер прерываний.
    fn init(&mut self) -> Result<(), InterruptControllerInitializationError>;

    /// Включает указанное прерывание.
    fn enable(&mut self, irq: IrqNumber);

    /// Отключает указанное прерывание.
    fn disable(&mut self, irq: IrqNumber);

    /// Устанавливает приоритет для прерывания.
    fn set_priority(&mut self, irq: IrqNumber, priority: IrqPriority);

    /// Устанавливает целевые CPU для маршрутизации прерывания (только для SPI).
    fn set_target(&mut self, irq: IrqNumber, target: CpuMask);

    /// Подтверждает прерывание и возвращает его номер.
    ///
    /// Возвращает `None` для spurious interrupt (1023 для GICv2).
    fn acknowledge(&self) -> Option<IrqNumber>;

    /// Сигнализирует об окончании обработки прерывания (EOI).
    fn end_of_interrupt(&self, irq: IrqNumber);
}
