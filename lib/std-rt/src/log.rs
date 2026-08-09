//! Строки процесса в системный журнал.
//!
//! Лог-сессия приходит процессу стартовым кадром компонента, и до неё строки уходят в никуда.
//! Тег источника пуст: имя источника сервер контракта берёт из значка сессии.

use core::sync::atomic::{AtomicU32, Ordering};

use ipc::wire::IpcError;
use log_abi::{LOG_MESSAGE_MAX, LineAssembler, log};
use syscall::Handle;

use crate::{PortTransport, lock::Lock, svc};

const EMPTY_TAG: &str = "";

static SESSION: AtomicU32 = AtomicU32::new(0);

/// Поток вывода, ведущий записи журнала построчно.
///
/// Запись журнала заводится по переводу строки, незавершённую строку отдаёт [`Stream::flush`].
pub struct Stream {
    lines: Lock<LineAssembler<LOG_MESSAGE_MAX>>,
}

impl Stream {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            lines: Lock::new(LineAssembler::new()),
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
        let mut failure = None;
        take(&mut self.lines.lock(), &mut |line| {
            // Отказ первой записи возвращается вызывающему, остальные строки уходят следом:
            // накопитель уже отдал их, и держать их негде.
            let outcome = client.write_str(EMPTY_TAG, line);
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

/// Запоминает лог-сессию процесса.
///
/// Сессию приносит стартовый кадр, и он один: лишнюю капу вызов закрывает.
pub fn install(handle: Handle) {
    if SESSION
        .compare_exchange(0, handle.raw(), Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        svc::handle_close(handle);
    }
}

/// Пишет текст в журнал построчно, не накапливая незавершённую строку.
///
/// Путь для вывода, которому накопитель недоступен: сообщения паники идут мимо потоков.
pub fn write(text: &str) -> Result<(), IpcError> {
    let Some(client) = client() else {
        return Ok(());
    };
    for line in text.strip_suffix('\n').unwrap_or(text).split('\n') {
        client.write_str(EMPTY_TAG, line)?;
    }
    Ok(())
}

fn client() -> Option<log::Client<PortTransport>> {
    let handle = Handle::new(SESSION.load(Ordering::Acquire))?;
    Some(log::Client::new(PortTransport::client(handle)))
}
