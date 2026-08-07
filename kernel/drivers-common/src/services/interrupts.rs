//! Контракты подсистемы прерываний.

use alloc::boxed::Box;

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

/// Полоса линий, которые контроллер платформы отдаёт наружу: включительные
/// границы. Приватные для CPU линии в неё не входят.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IrqLineSpan {
    pub first: IrqNumber,
    pub last: IrqNumber,
}

/// Приоритет прерывания (0 = наивысший, 255 = наинизший).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct IrqPriority(u8);

impl IrqPriority {
    /// Наивысший приоритет.
    pub const HIGHEST: Self = Self(0);

    /// Создаёт приоритет из сырого значения.
    pub const fn new(priority: u8) -> Self {
        Self(priority)
    }

    /// Возвращает сырое значение приоритета.
    pub const fn raw(self) -> u8 {
        self.0
    }
}

/// Тип триггера линии прерывания.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerType {
    /// Триггер по фронту
    Edge,

    /// Триггер по уровню
    Level,
}

/// Маска целевых CPU для маршрутизации прерываний.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct CpuMask(u8);

impl CpuMask {
    /// Все CPU.
    pub const ALL: Self = Self(0xFF);

    /// CPU 0.
    pub const CPU0: Self = Self(0b0001);

    /// Создаёт маску с единственным CPU по его индексу.
    pub fn cpu(cpu_index: u8) -> Option<Self> {
        1u8.checked_shl(u32::from(cpu_index)).map(Self)
    }

    /// Возвращает сырое значение маски.
    pub const fn raw(self) -> u8 {
        self.0
    }
}

pub struct IrqBound {
    cleanup: Option<Box<dyn FnOnce() + Send>>,
}

impl IrqBound {
    /// RAII-развязка привязанной линии: дроп исполняет `cleanup`.
    pub fn new<F>(cleanup: F) -> Self
    where
        F: FnOnce() + Send + 'static,
    {
        Self {
            cleanup: Some(Box::new(cleanup)),
        }
    }
}

impl Drop for IrqBound {
    fn drop(&mut self) {
        if let Some(cleanup) = self.cleanup.take() {
            cleanup();
        }
    }
}

/// Ошибки регистрации обработчика IRQ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrqRegistrationError {
    /// Некорректный номер IRQ.
    InvalidIrq,
    /// Обработчик для данного IRQ уже существует.
    AlreadyRegistered,
    /// Регистрация не поддерживается текущим окружением.
    Unsupported,
    /// Прочая ошибка.
    Other(&'static str),
}

/// Адрес CPU в понятиях контроллера прерываний.
///
/// Контроллер адресует CPU по-своему, и с нумерацией ядра это не связано:
/// значение для него готовит платформенный слой: на ARM это affinity целевого
/// процессора.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct IpiTarget(u64);

impl IpiTarget {
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Ошибки отправки межпроцессорного прерывания.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpiError {
    /// Контроллер не умеет адресовать другой CPU.
    Unsupported,
    /// Номер вне диапазона межпроцессорных прерываний.
    NotAnIpi,
    /// Адрес не выражается в адресации этого контроллера.
    UnreachableTarget,
}

impl core::fmt::Display for IpiError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            IpiError::Unsupported => f.write_str("the controller cannot address another CPU"),
            IpiError::NotAnIpi => f.write_str("the interrupt number is not an IPI"),
            IpiError::UnreachableTarget => f.write_str("the controller cannot address that CPU"),
        }
    }
}

/// Ошибки настройки per-CPU части контроллера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuInterfaceError {
    /// Контроллер не нашёл per-CPU части, принадлежащей текущему CPU.
    NotFound,
}

impl core::fmt::Display for CpuInterfaceError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            CpuInterfaceError::NotFound => {
                f.write_str("the controller has no per-CPU interface for this CPU")
            }
        }
    }
}

/// Контракт обработчика IRQ.
///
/// Реализация должна выдерживать реентрантный вызов с другого стека.
pub trait IrqHandler: Send + Sync {
    /// Вызывается при срабатывании соответствующего IRQ.
    fn handle(&self);
}

pub struct IrqBinding {
    pub irq: IrqNumber,

    /// `Some` - сконфигурировать ICFGR линии под этот триггер (до enable);
    /// `None` - не трогать ICFGR (линия наследует boot-конфигурацию).
    pub trigger: Option<TriggerType>,

    pub priority: IrqPriority,
    pub target: CpuMask,
    pub handler: Box<dyn IrqHandler>,
}

impl IrqBinding {
    pub fn new(
        irq: IrqNumber,
        trigger: Option<TriggerType>,
        priority: IrqPriority,
        target: CpuMask,
        handler: Box<dyn IrqHandler>,
    ) -> Self {
        Self {
            irq,
            trigger,
            priority,
            target,
            handler,
        }
    }
}

/// Контракт сервиса прерываний.
/// Определяет базовые операции для управления аппаратным контроллером прерываний.
pub trait InterruptsService: Send + Sync {
    /// Глобально включает прерывания.
    fn enable(&self);

    /// Глобально отключает прерывания.
    fn disable(&self);

    /// Регистрирует обработчик, настраивает IRQ и возвращает RAII-объект.
    fn bind(&self, binding: IrqBinding) -> Result<IrqBound, IrqRegistrationError>;

    /// Полоса линий контроллера, доступных для выдачи наружу.
    fn assignable_lines(&self) -> IrqLineSpan;

    /// Маскирует (запрещает) линию `irq` на контроллере, не снимая привязку.
    /// Используется уровневым IRQ-протоколом: линия маскируется при срабатывании и размаскируется
    /// на `ack`.
    fn mask(&self, irq: IrqNumber);

    /// Снимает маску (разрешает) линию `irq` на контроллере.
    fn unmask(&self, irq: IrqNumber);

    /// Обрабатывает IRQ (не более одного за раз).
    fn dispatch_interrupt(&self);

    /// Настраивает per-CPU часть контроллера на текущем CPU. Вызывается каждым
    /// CPU при старте.
    fn init_this_cpu(&self) -> Result<(), CpuInterfaceError>;

    /// Отправляет межпроцессорное прерывание `irq` процессору `target`.
    ///
    /// Линия должна быть привязана как обычная: доставленное прерывание
    /// приходит на общий путь диспетчеризации.
    fn send_ipi(&self, _irq: IrqNumber, _target: IpiTarget) -> Result<(), IpiError> {
        Err(IpiError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn irq_bound_runs_cleanup_once_on_drop() {
        static CLEANUP_CALLS: AtomicUsize = AtomicUsize::new(0);

        let bound = IrqBound::new(|| {
            CLEANUP_CALLS.fetch_add(1, Ordering::SeqCst);
        });

        drop(bound);

        assert_eq!(CLEANUP_CALLS.load(Ordering::SeqCst), 1);
    }

    struct SpyInterruptsService {
        mask_calls: AtomicUsize,
        unmask_calls: AtomicUsize,
    }

    impl SpyInterruptsService {
        fn new() -> Self {
            Self {
                mask_calls: AtomicUsize::new(0),
                unmask_calls: AtomicUsize::new(0),
            }
        }
    }

    impl InterruptsService for SpyInterruptsService {
        fn assignable_lines(&self) -> IrqLineSpan {
            IrqLineSpan {
                first: IrqNumber::new(32),
                last: IrqNumber::new(255),
            }
        }

        fn init_this_cpu(&self) -> Result<(), CpuInterfaceError> {
            Ok(())
        }

        fn enable(&self) {}

        fn disable(&self) {}

        fn bind(&self, _binding: IrqBinding) -> Result<IrqBound, IrqRegistrationError> {
            Ok(IrqBound::new(|| {}))
        }

        fn mask(&self, _irq: IrqNumber) {
            self.mask_calls.fetch_add(1, Ordering::SeqCst);
        }

        fn unmask(&self, _irq: IrqNumber) {
            self.unmask_calls.fetch_add(1, Ordering::SeqCst);
        }

        fn dispatch_interrupt(&self) {}
    }

    #[test]
    fn spy_records_mask_and_unmask_round_trip() {
        let service = SpyInterruptsService::new();
        let irq = IrqNumber::new(40);

        service.mask(irq);
        service.unmask(irq);

        assert_eq!(service.mask_calls.load(Ordering::SeqCst), 1);
        assert_eq!(service.unmask_calls.load(Ordering::SeqCst), 1);
    }
}
