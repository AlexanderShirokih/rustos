//! Адаптеры сервисов ARM Generic Timer.

use super::state::ArmGenericTimerState;
use alloc::sync::Arc;
use drivers_common::services::interrupts::IrqHandler;
use drivers_common::services::timer::TimerService;

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
    fn time_monotonic_elapsed(&self) -> u64 {
        self.state.get_elapsed_ns()
    }

    fn set_periodic(&self, interval_ms: u64) {
        self.state.set_periodic(interval_ms);
    }
}
