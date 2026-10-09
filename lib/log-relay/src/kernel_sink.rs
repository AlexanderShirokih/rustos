//! Сток relay в klog: строки уходят ядру по bootstrap-контракту.

use bootstrap_abi::bootstrap;
use ipc::{
    schema::{WireType, max_body_len},
    wire::{BODY_MAX, IpcError},
};
use runtime::PortTransport;
use syscall::{Handle, kind};

use crate::relay::LogSink;

const _: () = assert!(
    max_body_len(&[
        WireType::BoundedStr(log_abi::LOG_SOURCE_MAX),
        WireType::BoundedStr(log_abi::LOG_MESSAGE_MAX),
    ]) <= BODY_MAX,
    "the bootstrap log frame holds a log source and a log message without truncation"
);

/// Сток поверх bootstrap-капы ядра. Капа остаётся в процессе-владельце relay и
/// сохраняется дольше цикла пересылки: клиентам лог-порта она недоступна.
pub struct KernelLogSink {
    bootstrap: Handle<kind::Port>,
}

impl KernelLogSink {
    pub fn new(bootstrap: Handle<kind::Port>) -> Self {
        Self { bootstrap }
    }
}

impl LogSink for KernelLogSink {
    fn write(&mut self, source: &str, message: &str) -> Result<(), IpcError> {
        bootstrap::Client::new(PortTransport::new(self.bootstrap)).log_str(source, message)
    }
}
