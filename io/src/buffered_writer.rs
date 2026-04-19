//! Буферизированный Writer: накапливает вывод до подключения реального устройства.
//!
//! Используется для логирования до инициализации UART - данные пишутся в буфер,
//! при вызове `attach()` буфер сбрасывается в реальный writer.

extern crate alloc;

use alloc::{sync::Arc, vec::Vec};

use spin::Mutex;

use crate::writer::Writer;

/// Состояние буфера: накопление или прямая запись в writer.
enum BufferState {
    /// Накопление данных в памяти.
    Buffering(Vec<u8>),
    /// Прямая запись в подключённый writer.
    Attached(Arc<dyn Writer + Send + Sync>),
}

/// Writer, буферизующий вывод до подключения реального устройства.
///
/// Пока `attach()` не вызван, все данные накапливаются в `Vec<u8>`.
/// После `attach(writer)` накопленное сбрасывается в writer, дальнейшая запись идёт напрямую.
pub struct BufferedWriter {
    state: Mutex<BufferState>,
}

impl BufferedWriter {
    /// Создаёт новый буферизированный writer в режиме накопления.
    pub fn new() -> Self {
        Self {
            state: Mutex::new(BufferState::Buffering(Vec::new())),
        }
    }

    /// Подключает реальный writer: сбрасывает буфер в него и переключается на прямую запись.
    pub fn attach(&self, writer: Arc<dyn Writer + Send + Sync>) {
        let old = {
            let mut state = self.state.lock();
            core::mem::replace(&mut *state, BufferState::Attached(Arc::clone(&writer)))
        };
        if let BufferState::Buffering(buf) = old
            && !buf.is_empty()
        {
            writer.write_all(&buf);
            writer.flush();
        }
    }
}

impl Default for BufferedWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl Writer for BufferedWriter {
    fn write_all(&self, buf: &[u8]) {
        let mut state = self.state.lock();
        match &mut *state {
            BufferState::Buffering(buffer) => {
                buffer.extend_from_slice(buf);
            }
            BufferState::Attached(writer) => {
                let writer = Arc::clone(writer);
                drop(state);
                writer.write_all(buf);
            }
        }
    }

    fn flush(&self) {
        let state = self.state.lock();
        if let BufferState::Attached(writer) = &*state {
            let writer = Arc::clone(writer);
            drop(state);
            writer.flush();
        }
    }
}
