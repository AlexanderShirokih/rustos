//! Строки процесса в системный журнал по лог-капе из стартового кадра.

use core::fmt::Write as _;

use log_abi::{LOG_MESSAGE_MAX, LogBuffer, log};
use spin::Once;

use crate::{PortTransport, handle::OwnedHandle};

static LOG: Once<(OwnedHandle, &'static str)> = Once::new();

/// Запоминает лог-капу процесса и тег его строк. Повторный вызов ничего не
/// меняет: капу приносит стартовый кадр, а он один.
pub fn install_log(handle: OwnedHandle, tag: &'static str) {
    LOG.call_once(|| (handle, tag));
}

/// Best-effort строка в журнал. До стартового кадра - no-op.
pub fn log_line(message: &str) {
    let Some((handle, tag)) = LOG.get() else {
        return;
    };
    let client = log::Client::new(PortTransport::client(handle));
    let _ = client.write_str(tag, message);
}

/// Форматирует строку на стеке и отправляет её [`log_line`]. Строка длиннее
/// кадра уходит несколькими кадрами.
pub fn log_fmt(args: core::fmt::Arguments<'_>) {
    let mut writer = LineWriter {
        buffer: LogBuffer::new(),
    };
    let _ = writer.write_fmt(args);
    log_line(writer.buffer.as_str());
}

struct LineWriter {
    buffer: LogBuffer<LOG_MESSAGE_MAX>,
}

impl core::fmt::Write for LineWriter {
    fn write_str(&mut self, text: &str) -> core::fmt::Result {
        let mut rest = text;
        while !rest.is_empty() {
            let taken = self.buffer.push_str(rest);
            rest = &rest[taken..];
            if rest.is_empty() {
                break;
            }
            if taken == 0 && self.buffer.is_empty() {
                // Символ не помещается даже в пустой накопитель: кадр его не несёт.
                break;
            }
            log_line(self.buffer.as_str());
            self.buffer.clear();
        }
        Ok(())
    }
}
