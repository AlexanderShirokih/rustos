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

pub struct IrqBound {
    irq: Option<IrqNumber>,
    cleanup: Option<Box<dyn FnOnce() + Send>>,
}

impl IrqBound {
    /// Создаёт RAII-объект для уже зарегистрированного IRQ.
    pub fn new<F>(irq: IrqNumber, cleanup: F) -> Self
    where
        F: FnOnce() + Send + 'static,
    {
        let cleanup = Box::new(cleanup);

        Self {
            irq: Some(irq),
            cleanup: Some(cleanup),
        }
    }

    pub fn bind(
        handle: &dyn InterruptsService,
        binding: IrqBinding,
    ) -> Result<Self, IrqRegistrationError> {
        handle.bind(binding)
    }

    pub fn irq(&self) -> Option<IrqNumber> {
        self.irq
    }
}

impl Drop for IrqBound {
    fn drop(&mut self) {
        if let Some(cleanup) = self.cleanup.take() {
            cleanup();
        }

        self.irq = None;
    }
}

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
/// Реализация должна выдерживать реентрантный вызов с другого стека.
pub trait IrqHandler: Send + Sync {
    /// Вызывается при срабатывании соответствующего IRQ.
    fn handle(&self);
}

pub struct IrqBinding {
    pub irq: IrqNumber,
    pub priority: IrqPriority,
    pub target: CpuMask,
    pub handler: Box<dyn IrqHandler>,
}

impl IrqBinding {
    pub fn new(
        irq: IrqNumber,
        priority: IrqPriority,
        target: CpuMask,
        handler: Box<dyn IrqHandler>,
    ) -> Self {
        Self {
            irq,
            priority,
            target,
            handler,
        }
    }
}

/// Контракт сервиса прерываний.
///
/// Трейт определяет базовые операции для управления аппаратным контроллером прерываний.
pub trait InterruptsService: Send + Sync {
    /// Глобально включает прерывания.
    fn enable(&self);

    /// Глобально отключает прерывания.
    fn disable(&self);

    /// Регистрирует обработчик, настраивает IRQ и возвращает RAII-объект.
    fn bind(&self, binding: IrqBinding) -> Result<IrqBound, IrqRegistrationError>;

    /// Обрабатывает не более одного IRQ за вызов.
    fn dispatch_interrupt(&self);
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct TestIrqHandler;
    impl IrqHandler for TestIrqHandler {
        fn handle(&self) {}
    }

    #[test]
    fn irq_bound_runs_cleanup_once_on_drop() {
        static CLEANUP_CALLS: AtomicUsize = AtomicUsize::new(0);

        let bound = IrqBound::new(IrqNumber::new(40), || {
            CLEANUP_CALLS.fetch_add(1, Ordering::SeqCst);
        });

        assert_eq!(bound.irq(), Some(IrqNumber::new(40)));
        drop(bound);

        assert_eq!(CLEANUP_CALLS.load(Ordering::SeqCst), 1);
    }

    struct SpyInterruptsService {
        bind_calls: AtomicUsize,
    }

    impl SpyInterruptsService {
        fn new() -> Self {
            Self {
                bind_calls: AtomicUsize::new(0),
            }
        }
    }

    impl InterruptsService for SpyInterruptsService {
        fn enable(&self) {}

        fn disable(&self) {}

        fn bind(&self, binding: IrqBinding) -> Result<IrqBound, IrqRegistrationError> {
            self.bind_calls.fetch_add(1, Ordering::SeqCst);
            Ok(IrqBound::new(binding.irq, || {}))
        }

        fn dispatch_interrupt(&self) {}
    }

    #[test]
    fn irq_bound_bind_delegates_to_service_bind() {
        let service = SpyInterruptsService::new();
        let irq = IrqNumber::new(32);

        let bound = IrqBound::bind(
            &service,
            IrqBinding::new(
                irq,
                IrqPriority::LOWEST,
                CpuMask::ALL,
                Box::new(TestIrqHandler),
            ),
        )
        .expect("bind must succeed");

        assert_eq!(service.bind_calls.load(Ordering::SeqCst), 1);
        assert_eq!(bound.irq(), Some(irq));
    }

    struct FailingInterruptsService;

    impl InterruptsService for FailingInterruptsService {
        fn enable(&self) {}

        fn disable(&self) {}

        fn bind(&self, _binding: IrqBinding) -> Result<IrqBound, IrqRegistrationError> {
            Err(IrqRegistrationError::Unsupported)
        }

        fn dispatch_interrupt(&self) {}
    }

    #[test]
    fn irq_bound_bind_propagates_service_error() {
        let service = FailingInterruptsService;
        let result = IrqBound::bind(
            &service,
            IrqBinding::new(
                IrqNumber::new(33),
                IrqPriority::LOWEST,
                CpuMask::ALL,
                Box::new(TestIrqHandler),
            ),
        );

        assert!(matches!(result, Err(IrqRegistrationError::Unsupported)));
    }
}
