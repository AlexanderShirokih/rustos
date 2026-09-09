//! Сток relay в klog: строки уходят ядру по bootstrap-контракту.

use bootstrap_abi::bootstrap;
use ipc::wire::IpcError;
use runtime::PortTransport;
use syscall::{Handle, kind};

use crate::relay::LogSink;

const _: () = assert!(
    log_abi::LOG_SOURCE_MAX <= bootstrap_abi::LOG_TAG_MAX,
    "the bootstrap tag field holds a composed log source without truncation"
);
const _: () = assert!(
    log_abi::LOG_MESSAGE_MAX <= bootstrap_abi::LOG_MESSAGE_MAX,
    "the bootstrap message field holds a log message without truncation"
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
        bootstrap::Client::new(PortTransport::client(self.bootstrap)).log_str(source, message)
    }
}
