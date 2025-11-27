use crate::io::byte_sink::ByteSink;
use core::fmt::Write;

pub trait Writer: Write {
    fn write_all(&self, bytes: &[u8]);
    fn flush(&self) {}
}

pub struct BlockingWriter<S: ByteSink> {
    sink: S,
}

impl<S: ByteSink> BlockingWriter<S> {
    pub const fn new(sink: S) -> Self {
        Self { sink }
    }
}

impl<S: ByteSink> Write for BlockingWriter<S> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.write_all(s.as_bytes());
        self.flush();
        Ok(())
    }
}

impl<S: ByteSink> Writer for BlockingWriter<S> {
    fn write_all(&self, mut s: &[u8]) {
        while !s.is_empty() {
            match self.sink.try_write_slice(s) {
                Ok(n) if n > 0 => s = &s[n..],
                _ => core::hint::spin_loop(),
            }
        }
    }

    fn flush(&self) {
        self.sink.flush()
    }
}
