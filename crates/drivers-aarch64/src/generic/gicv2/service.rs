//! Адаптер InterruptsService для GICv2.

use alloc::sync::Arc;

use drivers_common::services::interrupts::{
    InterruptsService, IrqBinding, IrqBound, IrqRegistrationError,
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

        guard.handlers.insert(irq, Arc::from(handler));
        guard.enable(irq);

        let controller = self.controller.clone();
        let cleanup = move || {
            let mut guard = controller.lock();

            guard.disable(irq);
            guard.handlers.remove(&irq);
        };

        debug!("IRQ {irq:?} bound!");

        Ok(IrqBound::new(irq, cleanup))
    }

    fn dispatch_interrupt(&self) {
        let handler = self.controller.lock().dispatch_interrupt();

        if let Some(handler) = handler {
            handler.handle();
        }
    }
}
