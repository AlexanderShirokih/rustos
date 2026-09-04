//! Стековый накопитель строки лога и нарезка длинной строки на кадры.

use core::fmt::{Result as FmtResult, Write};

/// UTF-8 строка не длиннее `N` байт, собираемая по частям.
pub struct LogBuffer<const N: usize> {
    bytes: [u8; N],
    len: usize,
}

impl<const N: usize> LogBuffer<N> {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            bytes: [0; N],
            len: 0,
        }
    }

    /// Дописывает наибольший префикс `text`, укладывающийся в остаток границы,
    /// и возвращает его длину в байтах.
    pub fn push_str(&mut self, text: &str) -> usize {
        let take = fitting_prefix(text, N - self.len);
        let (Some(target), Some(source)) = (
            self.bytes.get_mut(self.len..self.len + take),
            text.as_bytes().get(..take),
        ) else {
            return 0;
        };
        target.copy_from_slice(source);
        self.len += take;
        take
    }

    /// Опустошает накопитель.
    pub fn clear(&mut self) {
        self.len = 0;
    }

    /// Накоплена ли хоть одна часть.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Накопленная строка.
    #[must_use]
    pub fn as_str(&self) -> &str {
        let text = core::str::from_utf8(&self.bytes[..self.len]);
        debug_assert!(text.is_ok(), "log buffer holds valid UTF-8 by construction");
        text.unwrap_or("")
    }
}

impl<const N: usize> Default for LogBuffer<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Write for LogBuffer<N> {
    fn write_str(&mut self, text: &str) -> FmtResult {
        self.push_str(text);
        Ok(())
    }
}

/// Режет `text` на куски не длиннее `N` байт по границе символа. Пустая строка
/// даёт один пустой кусок.
pub fn chunks<const N: usize>(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text;
    let mut done = false;
    core::iter::from_fn(move || {
        if done {
            return None;
        }
        // Символ, не влезающий в N целиком, всё равно уезжает одним куском:
        // резать его нельзя, а терять - тем более.
        let take = match fitting_prefix(rest, N) {
            0 => rest.chars().next().map_or(0, char::len_utf8),
            fits => fits,
        };
        let (head, tail) = rest.split_at(take);
        rest = tail;
        done = rest.is_empty();
        Some(head)
    })
}

fn fitting_prefix(text: &str, room: usize) -> usize {
    if text.len() <= room {
        return text.len();
    }
    let mut len = room;
    while len > 0 && !text.is_char_boundary(len) {
        len -= 1;
    }
    len
}

#[cfg(test)]
mod tests {
    use core::fmt::Write as _;

    use super::LogBuffer;

    #[test]
    fn parts_join_in_order() {
        let mut buffer = LogBuffer::<16>::new();
        buffer.push_str("filesystem");
        buffer.push_str("/");
        buffer.push_str("ramfs");
        assert_eq!(buffer.as_str(), "filesystem/ramfs");
    }

    #[test]
    fn overflowing_part_is_cut_to_the_bound() {
        let mut buffer = LogBuffer::<8>::new();
        buffer.push_str("router");
        buffer.push_str("/imagefs");
        assert_eq!(buffer.as_str(), "router/i");
    }

    #[test]
    fn cut_lands_on_a_char_boundary() {
        // "é" занимает два байта, срез проходит по границе символа.
        let mut buffer = LogBuffer::<5>::new();
        buffer.push_str("éèêë");
        assert_eq!(buffer.as_str(), "éè");
    }

    #[test]
    fn full_buffer_takes_nothing_more() {
        let mut buffer = LogBuffer::<4>::new();
        buffer.push_str("abcd");
        buffer.push_str("efgh");
        assert_eq!(buffer.as_str(), "abcd");
    }

    #[test]
    fn chunks_join_back_into_the_source() {
        let text = "éèêë dans le brouillard";
        let joined: std::string::String = super::chunks::<4>(text).collect();
        assert_eq!(joined, text);
    }

    #[test]
    fn chunks_never_split_a_char() {
        for chunk in super::chunks::<3>("éèêë") {
            assert!(chunk.chars().count() >= 1);
        }
    }

    #[test]
    fn empty_text_yields_one_empty_chunk() {
        let chunks: std::vec::Vec<&str> = super::chunks::<8>("").collect();
        assert_eq!(chunks, [""]);
    }

    #[test]
    fn text_within_the_bound_is_one_chunk() {
        let chunks: std::vec::Vec<&str> = super::chunks::<8>("short").collect();
        assert_eq!(chunks, ["short"]);
    }

    #[test]
    fn formatted_tail_beyond_the_bound_is_dropped() {
        let mut buffer = LogBuffer::<10>::new();
        write!(buffer, "badge-{:016x}", 0x1234_u64).expect("write into a truncating buffer");
        assert_eq!(buffer.as_str(), "badge-0000");
    }
}
