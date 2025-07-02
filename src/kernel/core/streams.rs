/// Бесконечный поток чтения байтов
pub trait InputStream {
    type ReadError;

    fn read(&mut self) -> Result<u8, Self::ReadError>;
}

pub trait OutputStream {
    type WriteError;

    fn write(&mut self, byte: u8) -> Result<(), Self::WriteError>;
}

pub trait OutputStreamExt: OutputStream {
    fn write_str(&mut self, s: &str) -> Result<(), Self::WriteError> {
        for byte in s.bytes() {
            self.write(byte)?;
        }
        Ok(())
    }
}

impl<T: ?Sized + OutputStream> OutputStreamExt for T {}
