//! Relay логирования rootkeeper: сток - bootstrap-капа ядра, которая процесс не
//! покидает, а подсистемам достаются только лог-сессии.

use log_relay::{KernelLogSink, LogRelayThread};
use runtime::Result;
use syscall::Handle;

// Значок лог-сессии супервизора.
pub(crate) const SUPERVISOR_BADGE: u64 = 1;

// Заводит лог-порт и его выделенный поток. `kernel` - bootstrap-капа ядра:
// её видит только сток relay.
pub(crate) fn start(kernel: Handle) -> Result<LogRelayThread> {
    LogRelayThread::start(source_name, KernelLogSink::new(kernel))
}

fn source_name(badge: u64) -> Option<&'static str> {
    (badge == SUPERVISOR_BADGE).then_some("rootkeeper")
}
