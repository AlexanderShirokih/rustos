//! Буферизированный Writer: накапливает вывод до подключения реального устройства.
//!
//! Используется для логирования до инициализации UART - данные пишутся в буфер,
//! при вызове `attach()` буфер сбрасывается в реальный writer.

use alloc::{sync::Arc, vec::Vec};

use io::writer::Writer;
use spin::Mutex;

enum BufferState {
    Buffering(Vec<u8>),
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
    pub fn attach(&self, writer: &Arc<dyn Writer + Send + Sync>) {
        let old = {
            let mut state = self.state.lock();
            core::mem::replace(&mut *state, BufferState::Attached(Arc::clone(writer)))
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

#[cfg(test)]
mod tests {
    use alloc::{sync::Arc, vec::Vec};

    use io::writer::Writer;
    use spin::Mutex;

    use super::*;

    #[derive(Default)]
    struct RecordingWriter {
        written: Mutex<Vec<u8>>,
    }

    impl Writer for RecordingWriter {
        fn write_all(&self, buf: &[u8]) {
            self.written.lock().extend_from_slice(buf);
        }

        fn flush(&self) {}
    }

    fn attach_recorder(writer: &BufferedWriter) -> Arc<RecordingWriter> {
        let recorder = Arc::new(RecordingWriter::default());
        let as_writer: Arc<dyn Writer + Send + Sync> = recorder.clone();
        writer.attach(&as_writer);
        recorder
    }

    #[test]
    fn buffered_output_reaches_the_writer_on_attach() {
        let writer = BufferedWriter::new();
        writer.write_all(b"early");

        let recorder = attach_recorder(&writer);

        assert_eq!(recorder.written.lock().as_slice(), b"early");
    }
}
