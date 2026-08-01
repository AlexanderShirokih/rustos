//! Адаптер InterruptsService для GICv2.

use alloc::sync::Arc;

use collections::{LockCell, MutexCell};
use drivers_common::services::interrupts::{
    CpuInterfaceError, InterruptsService, IpiError, IpiTarget, IrqBinding, IrqBound, IrqNumber,
    IrqRegistrationError,
};
use klog::debug;

use super::{
    controller::{BoundIrq, Gicv2Controller},
    regs::IrqType,
};

pub(super) struct GicInterruptsService {
    pub(super) controller: Arc<MutexCell<Gicv2Controller>>,
}

impl GicInterruptsService {
    pub(super) fn new(controller: Arc<MutexCell<Gicv2Controller>>) -> Self {
        Self { controller }
    }
}

impl InterruptsService for GicInterruptsService {
    fn enable(&self) {
        self.controller
            .with_lock(|_| Gicv2Controller::enable_global());
    }

    fn disable(&self) {
        self.controller
            .with_lock(|_| Gicv2Controller::disable_global());
    }

    fn init_this_cpu(&self) -> Result<(), CpuInterfaceError> {
        self.controller.with_lock(Gicv2Controller::init_this_cpu)
    }

    fn send_ipi(&self, irq: IrqNumber, target: IpiTarget) -> Result<(), IpiError> {
        self.controller
            .with_lock(|controller| controller.send_sgi(irq, target))
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
            controller.set_target_cpu(irq, target);

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

    fn dispatch_interrupt(&self) {
        super::super::dispatch_interrupt(&self.controller);
    }
}
