//! Адаптер InterruptsService для GICv2.

use alloc::sync::Arc;

use drivers_common::services::interrupts::{
    InterruptsService, IrqBinding, IrqBound, IrqNumber, IrqRegistrationError,
};
use klog::debug;
use spin::Mutex;

use super::{controller::Gicv2Controller, regs::IrqType};

pub(super) struct GicInterruptsService {
    pub(super) controller: Arc<Mutex<Gicv2Controller>>,
}

impl GicInterruptsService {
    pub(super) fn new(controller: Arc<Mutex<Gicv2Controller>>) -> Self {
        Self { controller }
    }
}

impl InterruptsService for GicInterruptsService {
    fn enable(&self) {
        let _guard = self.controller.lock();
        Gicv2Controller::enable_global();
    }

    fn disable(&self) {
        let _guard = self.controller.lock();
        Gicv2Controller::disable_global();
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
        guard.set_target_cpu(irq, target);

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
        self.controller.lock().enable(irq);
    }

    fn dispatch_interrupt(&self) {
        super::super::dispatch_interrupt(&self.controller);
    }
}
