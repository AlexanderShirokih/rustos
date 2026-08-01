//! Строки сервера в klog по каналу логирования из стартового кадра.

use core::fmt::Write as _;

use bootstrap::{BootstrapClient, LOG_MESSAGE_MAX};
use spin::Once;

use crate::{handle::OwnedHandle, port_transport::PortTransport};

// Канал логирования и тег строк живут дольше цикла обслуживания: логируется и его ранний отказ.
static LOG: Once<(OwnedHandle, &'static str)> = Once::new();

/// Запоминает канал логирования и тег процесса. Повторный вызов ничего не
/// меняет: канал приносит стартовый кадр, а он один.
pub fn install_log(handle: OwnedHandle, tag: &'static str) {
    LOG.call_once(|| (handle, tag));
}

/// Best-effort строка в klog. До стартового кадра - no-op.
pub fn log_line(message: &str) {
    let Some((handle, tag)) = LOG.get() else {
        return;
    };
    let client = BootstrapClient::new(PortTransport::client(handle));
    let _ = client.log_str(tag, message);
}

/// Форматирует строку на стеке и отправляет её [`log_line`]. Излишек сверх
/// ёмкости кадра отбрасывается.
pub fn log_fmt(args: core::fmt::Arguments<'_>) {
    let mut buffer = MessageBuffer {
        len: 0,
        bytes: [0; LOG_MESSAGE_MAX],
    };
    let _ = buffer.write_fmt(args);
    if let Ok(text) = core::str::from_utf8(&buffer.bytes[..buffer.len]) {
        log_line(text);
    }
}

// Стековый накопитель строки лога.
struct MessageBuffer {
    len: usize,
    bytes: [u8; LOG_MESSAGE_MAX],
}

impl core::fmt::Write for MessageBuffer {
    fn write_str(&mut self, text: &str) -> core::fmt::Result {
        // Хвост отбрасывается по границе символа: символ, разрезанный посередине,
        // ломает UTF-8 всей накопленной строки.
        let take = fitting_prefix(text, LOG_MESSAGE_MAX - self.len);
        self.bytes[self.len..self.len + take].copy_from_slice(&text.as_bytes()[..take]);
        self.len += take;
        Ok(())
    }
}

// Длина наибольшего префикса text, укладывающегося в room байт по границе символа.
fn fitting_prefix(text: &str, room: usize) -> usize {
    if text.len() <= room {
        return text.len();
    }
    text.char_indices()
        .map(|(index, symbol)| index + symbol.len_utf8())
        .take_while(|&end| end <= room)
        .last()
        .unwrap_or(0)
}
