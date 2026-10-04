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

    pub(crate) fn position(&self) -> usize {
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

    pub(crate) fn read_bytes(&mut self, length: usize) -> Option<&'a [u8]> {
        let start = self.position;
        let end = start.checked_add(length)?;
        let bytes = self.buffer.get(start..end)?;
        self.position = end;
        Some(bytes)
    }

    pub(crate) fn read_cstr_here(&mut self) -> Option<&'a str> {
        let start = self.position;
        let text = cstr_at(self.buffer, start)?;
        self.position = start + text.len() + 1;
        Some(text)
    }
}

pub(crate) fn cstr_at(buffer: &[u8], offset: usize) -> Option<&str> {
    let tail = buffer.get(offset..)?;
    let len = tail.iter().position(|byte| *byte == 0)?;
    core::str::from_utf8(tail.get(..len)?).ok()
}
