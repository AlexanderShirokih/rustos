//! Общий lifecycle-стейт Process/Thread: exit-код + terminate-сигнал.

use core::sync::atomic::{AtomicI32, Ordering};

use super::wait::SignalState;

pub(crate) struct TerminationState {
    signals: SignalState,
    exit_code: AtomicI32,
}

impl TerminationState {
    pub(crate) fn new() -> Self {
        Self {
            signals: SignalState::new(0),
            exit_code: AtomicI32::new(0),
        }
    }

    /// Идемпотентно публикует `code` и поднимает `terminated_bit`.
    pub(crate) fn signal_terminated(&self, terminated_bit: u32, code: i32) {
        // Release/Acquire: код виден тому, кто увидел сигнал.
        if self.signals.peek() & terminated_bit != 0 {
            return;
        }
        self.exit_code.store(code, Ordering::Release);
        self.signals.signal(terminated_bit, 0);
    }

    pub(crate) fn exit_code(&self) -> i32 {
        self.exit_code.load(Ordering::Acquire)
    }

    pub(crate) fn signals(&self) -> &SignalState {
        &self.signals
    }

    pub(crate) fn peek(&self) -> u32 {
        self.signals.peek()
    }
}
