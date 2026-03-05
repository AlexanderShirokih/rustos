use core::fmt::Write;

use crate::byte_sink::ByteSink;

/// Трейт для потоковой записи данных.
pub trait Writer {
    /// Записывает все байты из буфера, блокируя до завершения.
    fn write_all(&self, buf: &[u8]);

    /// Ожидает завершения передачи всех данных.
    fn flush(&self);
}

/// Обёртка над `ByteSink`, превращающая неблокирующую запись в блокирующую.
///
/// При невозможности записи выполняет busy-wait через `spin_loop`.
pub struct BlockingWriter<'a, S: ByteSink> {
    /// Приёмник байтов для записи.
    sink: &'a S,
}

impl<'a, S: ByteSink> BlockingWriter<'a, S> {
    pub const fn new(sink: &'a S) -> Self {
        Self { sink }
    }
}

impl<S: ByteSink> Writer for BlockingWriter<'_, S> {
    fn write_all(&self, mut s: &[u8]) {
        while !s.is_empty() {
            match self.sink.try_write_slice(s) {
                Ok(n) if n > 0 => s = &s[n..],
                _ => core::hint::spin_loop(),
            }
        }
    }

    fn flush(&self) {
        self.sink.flush();
    }
}

impl<S: ByteSink> Write for BlockingWriter<'_, S> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.write_all(s.as_bytes());
        self.flush();
        Ok(())
    }
}
