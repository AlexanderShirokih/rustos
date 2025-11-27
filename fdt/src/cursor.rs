#[derive(Clone)]
pub(crate) struct Cursor<'a> {
    pub(crate) buffer: &'a [u8],
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

    pub(crate) fn read_u32(&mut self) -> u32 {
        let b = self.buffer;
        let p = self.position;

        self.position += 4;
        u32::from_be_bytes([b[p], b[p + 1], b[p + 2], b[p + 3]])
    }

    pub fn read_cstr_at(&self, offset: usize) -> &'a str {
        let mut end = offset;
        let buffer = self.buffer;

        while end < buffer.len() && buffer[end] != 0 {
            end += 1;
        }

        unsafe { core::str::from_utf8_unchecked(&buffer[offset..end]) }
    }

    pub fn read_cstr_here(&mut self) -> &'a str {
        let start = self.position;
        let mut end = start;
        let buf = self.buffer;
        while end < buf.len() && buf[end] != 0 {
            end += 1;
        }
        self.position = end + 1; // перескочить '\0'
        unsafe { core::str::from_utf8_unchecked(&buf[start..end]) }
    }
}
