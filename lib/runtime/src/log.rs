//! Строки процесса в системный журнал.
//!
//! Лог-сессия приходит процессу стартовым кадром компонента, и до неё строки уходят в никуда.
//! Тег источника процесс задаёт при установке сессии. Пустой тег оставляет имя источника
//! значку сессии, непустой сам называет источник.

use core::{
    fmt::Write as _,
    slice, str,
    sync::atomic::{AtomicU32, AtomicUsize, Ordering},
};

use ipc::wire::IpcError;
use log_abi::{LOG_MESSAGE_MAX, LineAssembler, LogBuffer, log};
use syscall::kind;

use crate::{
    svc::{IssuedHandle, PortTransport},
    sync::Mutex,
};

static SESSION: AtomicU32 = AtomicU32::new(0);

static TAG_ADDRESS: AtomicUsize = AtomicUsize::new(0);

static TAG_LENGTH: AtomicUsize = AtomicUsize::new(0);

/// Поток вывода, ведущий записи журнала построчно.
///
/// Запись журнала завершается по переводу строки, незавершённую строку отдаёт [`Stream::flush`].
pub struct Stream {
    lines: Mutex<LineAssembler<LOG_MESSAGE_MAX>>,
}

impl Stream {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            lines: Mutex::new(LineAssembler::new()),
        }
    }

    /// Принимает кусок вывода и отдаёт журналу завершённые им строки.
    pub fn write(&self, text: &str) -> Result<(), IpcError> {
        self.emit(|lines, sink| lines.push(text, sink))
    }

    /// Отдаёт журналу незавершённую строку потока.
    pub fn flush(&self) -> Result<(), IpcError> {
        self.emit(|lines, sink| lines.flush(sink))
    }

    fn emit(
        &self,
        take: impl FnOnce(&mut LineAssembler<LOG_MESSAGE_MAX>, &mut dyn FnMut(&str)),
    ) -> Result<(), IpcError> {
        let Some(client) = client() else {
            return Ok(());
        };
        let tag = tag();
        let mut failure = None;
        take(&mut self.lines.lock(), &mut |line| {
            // Отказ первой записи возвращается вызывающему, остальные строки уходят следом:
            // накопитель уже отдал их, и держать их негде.
            let outcome = client.write_str(tag, line);
            failure = failure.take().or_else(|| outcome.err());
        });
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl Default for Stream {
    fn default() -> Self {
        Self::new()
    }
}

/// Отвечает, поставлена ли лог-сессия процесса.
#[must_use]
pub fn installed() -> bool {
    SESSION.load(Ordering::Acquire) != 0
}

/// Запоминает лог-сессию процесса и тег его строк.
///
/// Сессию приносит стартовый кадр, и он один. Сессия остаётся открытой до завершения
/// процесса. Повторный вызов закрывает переданную сессию.
pub fn install(session: IssuedHandle<kind::Port>, tag: &'static str) {
    if SESSION
        .compare_exchange(
            0,
            session.as_raw().raw(),
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return;
    }
    let _ = session.leak();
    TAG_ADDRESS.store(tag.as_ptr() as usize, Ordering::Relaxed);
    // Длина публикуется последней, и по ней тег читают.
    TAG_LENGTH.store(tag.len(), Ordering::Release);
}

/// Пишет текст в журнал построчно, не накапливая незавершённую строку.
///
/// Накопитель этому пути вывода недоступен. Через него идут [`line()`] и [`fmt()`].
pub fn write(text: &str) -> Result<(), IpcError> {
    let Some(client) = client() else {
        return Ok(());
    };
    let tag = tag();
    for line in text.strip_suffix('\n').unwrap_or(text).split('\n') {
        client.write_str(tag, line)?;
    }
    Ok(())
}

fn tag() -> &'static str {
    let length = TAG_LENGTH.load(Ordering::Acquire);
    if length == 0 {
        return "";
    }
    let address = TAG_ADDRESS.load(Ordering::Relaxed);
    // SAFETY: адрес и длина принадлежат одному `&'static str`, положенному `install`; байты
    // остаются валидными до конца процесса, а непустая длина означает записанный адрес.
    let bytes = unsafe { slice::from_raw_parts(address as *const u8, length) };
    // SAFETY: срез покрывает байты `&str` целиком, поэтому содержит валидный UTF-8.
    unsafe { str::from_utf8_unchecked(bytes) }
}

fn client() -> Option<log::Client<PortTransport>> {
    let handle = crate::restore(SESSION.load(Ordering::Acquire))?;
    Some(log::Client::new(PortTransport::new(handle)))
}

/// Best-effort строка в журнал. До стартового кадра - no-op.
pub fn line(message: &str) {
    let _ = write(message);
}

/// Форматирует строку на стеке и отправляет её [`line()`]. Строка длиннее кадра уходит
/// несколькими кадрами.
pub fn fmt(args: core::fmt::Arguments<'_>) {
    let mut writer = LineWriter {
        buffer: LogBuffer::new(),
    };
    let _ = writer.write_fmt(args);
    line(writer.buffer.as_str());
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
                // Символ не помещается даже в пустой накопитель, кадр не вмещает его целиком.
                break;
            }
            line(self.buffer.as_str());
            self.buffer.clear();
        }
        Ok(())
    }
}
