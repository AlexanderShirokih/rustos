use core::fmt::{self, Write};

/// Строка фиксированной ёмкости на стеке.
///
/// При переполнении тихо обрезает вывод.
/// Реализует [`Write`]
///
/// # Пример
///
/// ```
/// use collections::StaticString;
/// use core::fmt::Write;
///
/// let mut buf = StaticString::<128>::new();
/// writeln!(buf, "{:#018x}", 0xDEAD_BEEFu64).ok();
/// ```
pub struct StaticString<const N: usize> {
    /// Буфер для хранения UTF-8 байт.
    buf: [u8; N],
    /// Количество записанных байт.
    len: usize,
}

impl<const N: usize> StaticString<N> {
    /// Создаёт пустую строку.
    pub const fn new() -> Self {
        Self {
            buf: [0; N],
            len: 0,
        }
    }

    pub fn as_str(&self) -> &str {
        // SAFETY: записываем только валидный UTF-8 через fmt::Write::write_str.
        unsafe { core::str::from_utf8_unchecked(&self.buf[..self.len]) }
    }

    /// Количество записанных байт.
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Возвращает `true`, если строка пуста.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Максимальная ёмкость в байтах.
    pub const fn capacity(&self) -> usize {
        N
    }

    /// Количество свободных байт.
    pub const fn remaining(&self) -> usize {
        N - self.len
    }

    pub fn clear(&mut self) {
        self.len = 0;
    }
}

impl<const N: usize> Default for StaticString<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Write for StaticString<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let bytes = s.as_bytes();
        let available = N - self.len;

        let to_copy = bytes.len().min(available);
        self.buf[self.len..self.len + to_copy].copy_from_slice(&bytes[..to_copy]);
        self.len += to_copy;

        Ok(())
    }
}

impl<const N: usize> fmt::Display for StaticString<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::fmt::Write;

    #[test]
    fn new_creates_empty_string() {
        let s = StaticString::<64>::new();
        assert!(s.is_empty(), "new string must be empty");
        assert_eq!(s.len(), 0);
        assert_eq!(s.as_str(), "");
    }

    #[test]
    fn write_str_appends_content() {
        let mut s = StaticString::<64>::new();
        write!(s, "hello").unwrap();
        assert_eq!(s.as_str(), "hello");
        assert_eq!(s.len(), 5);
    }

    #[test]
    fn write_fmt_formats_values() {
        let mut s = StaticString::<64>::new();
        write!(s, "x={:#06x}", 0xAB).unwrap();
        assert_eq!(s.as_str(), "x=0x00ab");
    }

    #[test]
    fn writeln_adds_newline() {
        let mut s = StaticString::<64>::new();
        writeln!(s, "line1").unwrap();
        writeln!(s, "line2").unwrap();
        assert_eq!(s.as_str(), "line1\nline2\n");
    }

    #[test]
    fn overflow_truncates_silently() {
        let mut s = StaticString::<8>::new();
        let result = write!(s, "0123456789ABCDEF");
        assert!(result.is_ok(), "overflow must not return an error");
        assert_eq!(s.as_str(), "01234567");
        assert_eq!(s.len(), 8);
    }

    #[test]
    fn overflow_preserves_partial_write() {
        let mut s = StaticString::<5>::new();
        write!(s, "abc").unwrap();
        write!(s, "defgh").unwrap();
        assert_eq!(s.as_str(), "abcde");
    }

    #[test]
    fn capacity_and_remaining_correct() {
        let mut s = StaticString::<32>::new();
        assert_eq!(s.capacity(), 32);
        assert_eq!(s.remaining(), 32);

        write!(s, "12345").unwrap();
        assert_eq!(s.remaining(), 27);
    }

    #[test]
    fn clear_resets_string() {
        let mut s = StaticString::<32>::new();
        write!(s, "hello").unwrap();
        s.clear();
        assert!(s.is_empty(), "string must be empty after clear");
        assert_eq!(s.as_str(), "");
    }

    #[test]
    fn display_outputs_content() {
        let mut s = StaticString::<32>::new();
        write!(s, "test").unwrap();

        let mut out = StaticString::<32>::new();
        write!(out, "{}", s).unwrap();
        assert_eq!(out.as_str(), "test");
    }
}
