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
                // Пишем '\r' сейчас, '\n' — в следующей итерации/вызове.
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
            w |= (byte as u32) << (i * 8);
        }
        (w, written, consumed)
    }

    /// Сколько исходных байт уже потреблено.
    #[inline]
    pub fn consumed(&self) -> usize {
        self.in_pos
    }
}

// Универсальный итератор по байтам с заменой LF → CRLF
impl<'a> Iterator for Crlf<'a> {
    type Item = u8;
    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let mut b = [0u8; 1];
        let (w, _c) = self.fill(&mut b);
        if w == 0 { None } else { Some(b[0]) }
    }
}
