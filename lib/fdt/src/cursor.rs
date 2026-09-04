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

    pub(crate) fn read_u32(&mut self) -> Option<u32> {
        let p = self.position;
        let word = *self.buffer.get(p..)?.first_chunk::<4>()?;
        self.position = p + 4;
        Some(u32::from_be_bytes(word))
    }

    pub fn read_cstr_at(&self, offset: usize) -> &'a str {
        let buffer = self.buffer;
        let Some(tail) = buffer.get(offset..) else {
            return "";
        };
        let len = tail
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(tail.len());
        tail.get(..len)
            .and_then(|text| core::str::from_utf8(text).ok())
            .unwrap_or("")
    }

    pub fn read_cstr_here(&mut self) -> &'a str {
        let start = self.position;
        let Some(tail) = self.buffer.get(start..) else {
            return "";
        };
        let len = tail
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(tail.len());
        self.position = start + len + 1; // перескочить '\0'
        tail.get(..len)
            .and_then(|text| core::str::from_utf8(text).ok())
            .unwrap_or("")
    }
}
