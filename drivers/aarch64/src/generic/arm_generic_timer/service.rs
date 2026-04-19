//! Адаптеры сервисов ARM Generic Timer.

use alloc::sync::Arc;

use drivers_common::services::{
    interrupts::IrqHandler,
    timer::{TickHandler, TimerService},
};

use super::state::ArmGenericTimerState;

pub(super) struct ArmGenericTimerIrqHandler {
    state: Arc<ArmGenericTimerState>,
}

impl ArmGenericTimerIrqHandler {
    pub(super) fn new(state: Arc<ArmGenericTimerState>) -> Self {
        Self { state }
    }
}

impl IrqHandler for ArmGenericTimerIrqHandler {
    fn handle(&self) {
        self.state.on_interrupt();
    }
}

pub(super) struct ArmGenericTimerHandle {
    state: Arc<ArmGenericTimerState>,
}

impl ArmGenericTimerHandle {
    pub(super) fn new(state: Arc<ArmGenericTimerState>) -> Self {
        Self { state }
    }
}

impl TimerService for ArmGenericTimerHandle {
    fn now_ns(&self) -> u64 {
        self.state.get_elapsed_ns()
    }

    fn schedule_next(&self, deadline_ns: u64) {
        self.state.schedule_next(deadline_ns);
    }

    fn set_handler(&self, handler: Arc<dyn TickHandler>) {
        self.state.set_handler(handler);
    }
}
