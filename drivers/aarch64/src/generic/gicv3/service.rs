//! Адаптер InterruptsService для GICv3.

use super::controller::Gicv3Controller;
use super::regs::IrqType;
use alloc::sync::Arc;
use drivers_common::services::interrupts::{
    InterruptsService, IrqBinding, IrqBound, IrqRegistrationError,
};
use klog::debug;
use spin::Mutex;

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
        self.controller.lock().enable_global();
    }

    fn disable(&self) {
        self.controller.lock().disable_global();
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
        guard.set_affinity(irq, target);
        guard.handlers.insert(irq, handler);
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
        self.controller.lock().dispatch_interrupt();
    }
}
