//! Адаптер InterruptsService для GICv3.

use alloc::sync::Arc;

use collections::{LockCell, MutexCell};
use drivers_common::services::interrupts::{
    CpuInterfaceError, InterruptsService, IpiError, IpiTarget, IrqBinding, IrqBound, IrqLineSpan,
    IrqNumber, IrqRegistrationError,
};
use klog::debug;

use super::{
    controller::{BoundIrq, Gicv3Controller},
    regs::{IrqType, LAST_SPI, LINES_PER_BANK, SPI_BASE},
};

pub(super) struct GicV3InterruptsService {
    pub(super) controller: Arc<MutexCell<Gicv3Controller>>,
}

impl GicV3InterruptsService {
    pub(super) fn new(controller: Arc<MutexCell<Gicv3Controller>>) -> Self {
        Self { controller }
    }
}

impl InterruptsService for GicV3InterruptsService {
    fn enable(&self) {
        self.controller
            .with_lock(|_| Gicv3Controller::enable_global());
    }

    fn disable(&self) {
        self.controller
            .with_lock(|_| Gicv3Controller::disable_global());
    }

    fn assignable_lines(&self) -> IrqLineSpan {
        let last = self
            .controller
            .with_lock(|controller| controller.interrupt_lines)
            .saturating_mul(LINES_PER_BANK)
            .saturating_sub(1);
        // INTID за LAST_SPI зарезервированы архитектурой и линиями не являются.
        let last = u16::try_from(last).unwrap_or(u16::MAX).min(LAST_SPI);
        IrqLineSpan {
            first: IrqNumber::new(SPI_BASE),
            last: IrqNumber::new(last),
        }
    }

    fn bind(&self, binding: IrqBinding) -> Result<IrqBound, IrqRegistrationError> {
        let IrqBinding {
            irq,
            trigger,
            priority,
            target,
            handler,
        } = binding;

        if matches!(IrqType::from_irq_number(irq), IrqType::Spurious) {
            return Err(IrqRegistrationError::InvalidIrq);
        }

        self.controller.with_lock(|controller| {
            if controller.handlers.contains_key(&irq) {
                return Err(IrqRegistrationError::AlreadyRegistered);
            }

            controller.set_priority(irq, priority);
            controller.set_affinity(irq, target);

            // Триггер - до enable: переконфиг включённой линии может быть непредсказуемым.
            if let Some(trigger) = trigger {
                controller.set_config(irq, trigger);
            }

            controller.handlers.insert(
                irq,
                BoundIrq {
                    handler: Arc::from(handler),
                    priority,
                    trigger,
                },
            );
            controller.enable(irq);
            Ok(())
        })?;

        let controller = self.controller.clone();
        let cleanup = move || {
            controller.with_lock(|controller| {
                controller.disable(irq);
                controller.handlers.remove(&irq);
            });
        };

        debug!("IRQ {irq:?} bound!");

        Ok(IrqBound::new(cleanup))
    }

    fn mask(&self, irq: IrqNumber) {
        self.controller
            .with_lock(|controller| controller.disable(irq));
    }

    fn unmask(&self, irq: IrqNumber) {
        super::super::barrier_before_unmask();
        self.controller
            .with_lock(|controller| controller.enable(irq));
    }

    fn init_this_cpu(&self) -> Result<(), CpuInterfaceError> {
        self.controller
            .with_lock(|controller| controller.init_this_cpu())
    }

    fn send_ipi(&self, irq: IrqNumber, target: IpiTarget) -> Result<(), IpiError> {
        // Лок контроллера не нужен: `ICC_SGI1R_EL1` - регистр текущего CPU, и
        // отображённых областей отправка не касается.
        Gicv3Controller::send_sgi(irq, target)
    }

    fn dispatch_interrupt(&self) {
        super::super::dispatch_interrupt(&self.controller);
    }
}
