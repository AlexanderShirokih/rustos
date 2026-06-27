//! Адаптер InterruptsService для GICv3.

use alloc::sync::Arc;

use drivers_common::services::interrupts::{
    InterruptsService, IrqBinding, IrqBound, IrqNumber, IrqRegistrationError,
};
use klog::debug;
use spin::Mutex;

use super::{controller::Gicv3Controller, regs::IrqType};

pub(super) struct GicV3InterruptsService {
    pub(super) controller: Arc<Mutex<Gicv3Controller>>,
}

impl GicV3InterruptsService {
    pub(super) fn new(controller: Arc<Mutex<Gicv3Controller>>) -> Self {
        Self { controller }
    }
}

impl InterruptsService for GicV3InterruptsService {
    fn enable(&self) {
        let _guard = self.controller.lock();
        Gicv3Controller::enable_global();
    }

    fn disable(&self) {
        let _guard = self.controller.lock();
        Gicv3Controller::disable_global();
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

        let mut guard = self.controller.lock();

        if guard.handlers.contains_key(&irq) {
            return Err(IrqRegistrationError::AlreadyRegistered);
        }

        guard.set_priority(irq, priority);
        guard.set_affinity(irq, target);

        // Триггер - до enable: переконфиг включённой линии может быть непредсказуемым.
        if let Some(trigger) = trigger {
            guard.set_config(irq, trigger);
        }

        guard.handlers.insert(irq, Arc::from(handler));
        guard.enable(irq);

        let controller = self.controller.clone();
        let cleanup = move || {
            let mut guard = controller.lock();
            guard.disable(irq);
            guard.handlers.remove(&irq);
        };

        debug!("IRQ {irq:?} bound!");

        Ok(IrqBound::new(cleanup))
    }

    fn mask(&self, irq: IrqNumber) {
        self.controller.lock().disable(irq);
    }

    fn unmask(&self, irq: IrqNumber) {
        super::super::barrier_before_unmask();
        self.controller.lock().enable(irq);
    }

    fn dispatch_interrupt(&self) {
        super::super::dispatch_interrupt(&self.controller);
    }
}
