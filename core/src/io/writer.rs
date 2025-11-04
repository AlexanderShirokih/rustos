use crate::io::byte_sink::ByteSink;
use core::fmt::Write;

pub trait Writer: Write {
    fn write_all(&self, bytes: &[u8]);
    fn flush(&self) {}
}

pub struct BlockingWriter<'a, S: ByteSink + ?Sized> {
    sink: &'a S,
}

impl<'a, S: ByteSink + ?Sized> BlockingWriter<'a, S> {
    pub const fn new(sink: &'a S) -> Self {
        Self { sink }
    }
}

impl<'a, S: ByteSink + ?Sized> Write for BlockingWriter<'a, S> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.write_all(s.as_bytes());
        self.flush();
        Ok(())
    }
}

impl<'a, S: ByteSink + ?Sized> Writer for BlockingWriter<'a, S> {
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
