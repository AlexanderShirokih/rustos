//! Сток relay в klog: строки уходят ядру по bootstrap-контракту.

use bootstrap::BootstrapClient;
use ipc::wire::IpcError;
use runtime::PortTransport;
use syscall::Handle;

use crate::relay::LogSink;

// Границы полей лог-кадра лежат в границах полей bootstrap-кадра: пересылка
// строку не усекает.
const _: () = assert!(log_abi::LOG_SOURCE_MAX <= bootstrap::LOG_TAG_MAX);
const _: () = assert!(log_abi::LOG_MESSAGE_MAX <= bootstrap::LOG_MESSAGE_MAX);

/// Сток поверх bootstrap-капы ядра. Капа остаётся в процессе-владельце relay и
/// живёт дольше цикла пересылки: клиентам лог-порта она недоступна.
pub struct KernelLogSink {
    bootstrap: Handle,
}

impl KernelLogSink {
    pub fn new(bootstrap: Handle) -> Self {
        Self { bootstrap }
    }
}

impl LogSink for KernelLogSink {
    fn write(&mut self, source: &str, message: &str) -> Result<(), IpcError> {
        BootstrapClient::new(PortTransport::client(self.bootstrap)).log_str(source, message)
    }
}
