//! Адаптер `capability::InterruptsControl` поверх `drivers_common`-сервиса.
//!
//! Единственное место, где встречаются архитектурно-независимый capability-слой и
//! drivers-common: capability говорит «сырыми» `u16`, адаптер заворачивает их в
//! `IrqNumber`, строит `IrqBinding` с мостом `IrqHandler -> IrqSink` и хранит
//! возвращённый `IrqBound` внутри `IrqBindToken`. Так capability-крейт остаётся
//! без зависимости на drivers-common.

use alloc::{boxed::Box, sync::Arc};

use capability::{InterruptsControl, IpcError, IrqBindToken, IrqSink};
use drivers_common::services::interrupts::{
    CpuMask, InterruptsService, IrqBinding, IrqHandler, IrqNumber, IrqPriority,
    IrqRegistrationError, TriggerType,
};
use syscall::IrqTrigger;

pub struct InterruptsControlAdapter {
    service: Arc<dyn InterruptsService>,
}

impl InterruptsControlAdapter {
    pub fn new(service: Arc<dyn InterruptsService>) -> Self {
        Self { service }
    }
}

struct SinkHandler {
    sink: Arc<dyn IrqSink>,
}

impl IrqHandler for SinkHandler {
    fn handle(&self) {
        // Вызов идёт в IRQ-контексте: `fire` синхронно маскирует линию и
        // поднимает `SIGNALED`.
        self.sink.fire();
    }
}

impl InterruptsControl for InterruptsControlAdapter {
    fn bind_line(
        &self,
        irq: u16,
        trigger: IrqTrigger,
        sink: Arc<dyn IrqSink>,
    ) -> Result<IrqBindToken, IpcError> {
        let binding = IrqBinding::new(
            IrqNumber::new(irq),
            Some(trigger_of(trigger)),
            IrqPriority::HIGHEST,
            CpuMask::ALL,
            Box::new(SinkHandler { sink }),
        );
        let bound = self.service.bind(binding).map_err(map_bind_error)?;
        // IrqBound (FnOnce-cleanup, не Clone) переезжает во владение токена;
        // его Drop отключит линию и удалит обработчик.
        Ok(IrqBindToken::new(bound))
    }

    fn mask(&self, irq: u16) {
        self.service.mask(IrqNumber::new(irq));
    }

    fn unmask(&self, irq: u16) {
        self.service.unmask(IrqNumber::new(irq));
    }
}

fn trigger_of(trigger: IrqTrigger) -> TriggerType {
    match trigger {
        IrqTrigger::Edge => TriggerType::Edge,
        IrqTrigger::Level => TriggerType::Level,
    }
}

fn map_bind_error(err: IrqRegistrationError) -> IpcError {
    match err {
        IrqRegistrationError::AlreadyRegistered => IpcError::AlreadyBound,
        IrqRegistrationError::InvalidIrq
        | IrqRegistrationError::Unsupported
        | IrqRegistrationError::Other(_) => IpcError::WrongType,
    }
}

#[cfg(test)]
mod tests {
    use collections::{LockCell, MutexCell};
    use drivers_common::services::interrupts::{
        CpuInterfaceError, IrqBound, IrqLineSpan, IrqRegistrationError, TriggerType,
    };

    use super::{
        Arc, InterruptsControl, InterruptsControlAdapter, InterruptsService, IrqBinding, IrqNumber,
        IrqSink, IrqTrigger,
    };

    struct SpyService {
        bound: MutexCell<Option<(IrqNumber, Option<TriggerType>)>>,
    }

    struct SilentSink;

    impl IrqSink for SilentSink {
        fn fire(&self) {}
    }

    impl InterruptsService for SpyService {
        fn assignable_lines(&self) -> IrqLineSpan {
            IrqLineSpan {
                first: IrqNumber::new(32),
                last: IrqNumber::new(255),
            }
        }

        fn enable(&self) {}

        fn disable(&self) {}

        fn bind(&self, binding: IrqBinding) -> Result<IrqBound, IrqRegistrationError> {
            self.bound
                .with_lock(|slot| *slot = Some((binding.irq, binding.trigger)));
            Ok(IrqBound::new(|| {}))
        }

        fn mask(&self, _irq: IrqNumber) {}

        fn unmask(&self, _irq: IrqNumber) {}

        fn dispatch_interrupt(&self) {}

        fn init_this_cpu(&self) -> Result<(), CpuInterfaceError> {
            Ok(())
        }
    }

    #[test]
    fn bound_line_carries_its_trigger_to_the_controller() {
        let service = Arc::new(SpyService {
            bound: MutexCell::new(None),
        });
        let adapter = InterruptsControlAdapter::new(service.clone());

        let bound = adapter.bind_line(37, IrqTrigger::Edge, Arc::new(SilentSink));

        assert!(bound.is_ok());
        assert_eq!(
            service.bound.with_lock(|slot| *slot),
            Some((IrqNumber::new(37), Some(TriggerType::Edge))),
            "edge line must reach the controller as edge"
        );
    }
}
