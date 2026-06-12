/// Адаптер: заменяет '\n' на "\r\n" при чтении из исходного среза.
pub struct Crlf<'a> {
    src: &'a [u8],
    in_pos: usize,
    need_lf: bool, // true, если ранее вывели '\r' и должны дописать '\n'
}

impl<'a> Crlf<'a> {
    #[inline]
    pub const fn new(src: &'a [u8]) -> Self {
        Self {
            src,
            in_pos: 0,
            need_lf: false,
        }
    }

    /// Заполняет `out` сконвертированными байтами.
    /// Возвращает (сколько_записано_в_out, сколько_потреблено_из_src).
    #[inline]
    pub fn fill(&mut self, out: &mut [u8]) -> (usize, usize) {
        let mut written = 0usize;
        let mut consumed = 0usize;

        while written < out.len() {
            // Дописать отложенный '\n'
            if self.need_lf {
                out[written] = b'\n';
                written += 1;
                self.need_lf = false;
                continue;
            }

            // Источник кончился
            if self.in_pos >= self.src.len() {
                break;
            }

            let b = self.src[self.in_pos];

            if b == b'\n' {
                // Не потребляем исходный байт, пока не выдали всю пару \r\n
                // Пишем '\r' сейчас, '\n' - в следующей итерации/вызове.
                out[written] = b'\r';
                written += 1;
                self.need_lf = true;
                // '\n' из src считаем потреблённым, как только началась пара
                self.in_pos += 1;
                consumed += 1;
            } else {
                out[written] = b;
                written += 1;
                self.in_pos += 1;
                consumed += 1;
            }
        }

        (written, consumed)
    }

    /// Упаковывает до 4 байт (LE) в u32 и возвращает (слово, байт_выхода, байт_входа).
    #[inline]
    pub fn pack_u32_le(&mut self) -> (u32, usize, usize) {
        let mut buf = [0u8; 4];
        let (written, consumed) = self.fill(&mut buf);
        let mut w: u32 = 0;
        for (i, &byte) in buf.iter().enumerate().take(written) {
            w |= u32::from(byte) << (i * 8);
        }
        (w, written, consumed)
    }

    /// Сколько исходных байт уже потреблено.
    #[inline]
    pub fn consumed(&self) -> usize {
        self.in_pos
    }
}

// Универсальный итератор по байтам с заменой LF -> CRLF
impl Iterator for Crlf<'_> {
    type Item = u8;
    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let mut b = [0u8; 1];
        let (w, _c) = self.fill(&mut b);
        if w == 0 { None } else { Some(b[0]) }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::vec::Vec;

    use super::Crlf;

    #[test]
    fn fill_copies_input_without_lf() {
        let mut crlf = Crlf::new(b"abc");
        let mut out = [0; 8];

        assert_eq!(crlf.fill(&mut out), (3, 3));
        assert_eq!(&out[..3], b"abc");
        assert_eq!(crlf.consumed(), 3);
        assert_eq!(crlf.fill(&mut out), (0, 0));
    }

    #[test]
    fn fill_expands_multiple_lf_to_crlf() {
        let mut crlf = Crlf::new(b"a\nb\n");
        let mut out = [0; 8];

        let (written, consumed) = crlf.fill(&mut out);

        assert_eq!((written, consumed), (6, 4));
        assert_eq!(&out[..written], b"a\r\nb\r\n");
        assert_eq!(crlf.consumed(), 4);
    }

    #[test]
    fn fill_with_one_byte_buffer_keeps_pending_lf() {
        let mut crlf = Crlf::new(b"\nX");
        let mut out = [0; 1];

        assert_eq!(crlf.fill(&mut out), (1, 1));
        assert_eq!(out[0], b'\r');
        assert_eq!(crlf.consumed(), 1);

        assert_eq!(crlf.fill(&mut out), (1, 0));
        assert_eq!(out[0], b'\n');
        assert_eq!(crlf.consumed(), 1);

        assert_eq!(crlf.fill(&mut out), (1, 1));
        assert_eq!(out[0], b'X');
        assert_eq!(crlf.consumed(), 2);
    }

    #[test]
    fn pack_u32_le_reports_written_and_consumed_bytes() {
        let mut crlf = Crlf::new(b"A\nB");

        assert_eq!(crlf.pack_u32_le(), (u32::from_le_bytes(*b"A\r\nB"), 4, 3));
        assert_eq!(crlf.pack_u32_le(), (0, 0, 0));
    }

    #[test]
    fn iterator_yields_expanded_stream_until_exhausted() {
        let bytes: Vec<u8> = Crlf::new(b"x\ny").collect();

        assert_eq!(bytes, b"x\r\ny");
    }
}
