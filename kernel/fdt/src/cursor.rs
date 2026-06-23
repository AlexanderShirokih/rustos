/// Курсор для последовательного чтения бинарных данных.
///
/// Предоставляет методы для чтения big-endian значений и C-строк.
#[derive(Clone)]
pub(crate) struct Cursor<'a> {
    /// Буфер с данными для чтения.
    pub(crate) buffer: &'a [u8],

    /// Текущая позиция чтения в буфере.
    position: usize,
}

impl<'a> Cursor<'a> {
    pub(crate) fn new(buffer: &'a [u8]) -> Self {
        Self {
            buffer,
            position: 0,
        }
    }

    pub(crate) fn set_position(&mut self, pos: usize) {
        self.position = pos.min(self.buffer.len());
    }

    pub fn position(&self) -> usize {
        self.position
    }

    pub(crate) fn align_up4(&mut self) {
        let p = (self.position + 3) & !3;
        self.set_position(p);
    }

    /// Читает big-endian `u32` из текущей позиции.
    ///
    /// Возвращает `None`, если в буфере недостаточно байт (обрезанный или
    /// недоверенный blob), чтобы не паниковать на OOB-доступе.
    pub(crate) fn read_u32(&mut self) -> Option<u32> {
        let p = self.position;
        let slice = self.buffer.get(p..p + 4)?;
        self.position = p + 4;
        Some(u32::from_be_bytes(slice.try_into().unwrap()))
    }

    pub fn read_cstr_at(&self, offset: usize) -> &'a str {
        let buffer = self.buffer;
        if offset >= buffer.len() {
            return "";
        }
        let mut end = offset;
        while end < buffer.len() && buffer[end] != 0 {
            end += 1;
        }
        core::str::from_utf8(&buffer[offset..end]).unwrap_or("")
    }

    pub fn read_cstr_here(&mut self) -> &'a str {
        let start = self.position;
        let buf = self.buffer;
        if start >= buf.len() {
            return "";
        }
        let mut end = start;
        while end < buf.len() && buf[end] != 0 {
            end += 1;
        }
        self.position = end + 1; // перескочить '\0'
        core::str::from_utf8(&buf[start..end]).unwrap_or("")
    }
}
