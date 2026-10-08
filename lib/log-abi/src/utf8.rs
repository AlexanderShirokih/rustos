//! Текст UTF-8 из байтового потока вывода.

use core::str;

const REPLACEMENT: &str = "\u{FFFD}";

const SEQUENCE_MAX: usize = 4;

/// Декодер байтового потока в текст UTF-8.
///
/// Декодер собирает последовательность, разорванную между кусками потока, и выдаёт её вместе со
/// следующим куском. Байты, которые не складываются в UTF-8, заменяются символом U+FFFD.
pub struct Utf8Assembler {
    pending: [u8; SEQUENCE_MAX],
    pending_len: usize,
}

impl Utf8Assembler {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            pending: [0; SEQUENCE_MAX],
            pending_len: 0,
        }
    }

    /// Декодирует `bytes` и отдаёт текст в `emit` кусками. Незавершённая последовательность в
    /// конце `bytes` ждёт следующего вызова.
    pub fn push(&mut self, bytes: &[u8], mut emit: impl FnMut(&str)) {
        let rest = self.complete_pending(bytes, &mut emit);
        self.decode(rest, &mut emit);
    }

    /// Завершает поток и выдаёт незавершённую последовательность символом U+FFFD.
    pub fn finish(&mut self, mut emit: impl FnMut(&str)) {
        if self.pending_len > 0 {
            self.pending_len = 0;
            emit(REPLACEMENT);
        }
    }

    fn complete_pending<'a>(&mut self, bytes: &'a [u8], emit: &mut impl FnMut(&str)) -> &'a [u8] {
        if self.pending_len == 0 {
            return bytes;
        }
        let held = self.pending_len;
        let mut joined = self.pending;
        let borrowed = bytes.len().min(SEQUENCE_MAX - held);
        let (start, _) = bytes.split_at(borrowed);
        let (_, free) = joined.split_at_mut(held);
        let (target, _) = free.split_at_mut(borrowed);
        target.copy_from_slice(start);
        let (joined, _) = joined.split_at(held + borrowed);
        self.pending_len = 0;

        let consumed = match str::from_utf8(joined) {
            Ok(text) => {
                emit(text);
                joined.len()
            }
            Err(error) if error.valid_up_to() > 0 => {
                let (valid, _) = joined.split_at(error.valid_up_to());
                emit(str::from_utf8(valid).unwrap_or_default());
                valid.len()
            }
            Err(error) => {
                // Кусок закончился раньше последовательности, и она остаётся отложенной целиком.
                let Some(invalid) = error.error_len() else {
                    self.hold(joined);
                    return &[];
                };
                emit(REPLACEMENT);
                invalid
            }
        };
        // Отложенные байты - начало одной последовательности, и разбор забирает их целиком.
        bytes
            .get(consumed.saturating_sub(held)..)
            .unwrap_or_default()
    }

    fn decode(&mut self, bytes: &[u8], emit: &mut impl FnMut(&str)) {
        let mut rest = bytes;
        loop {
            match str::from_utf8(rest) {
                Ok(text) => {
                    if !text.is_empty() {
                        emit(text);
                    }
                    return;
                }
                Err(error) => {
                    let (valid, after) = rest.split_at(error.valid_up_to());
                    if !valid.is_empty() {
                        emit(str::from_utf8(valid).unwrap_or_default());
                    }
                    let Some(invalid) = error.error_len() else {
                        self.hold(after);
                        return;
                    };
                    emit(REPLACEMENT);
                    rest = after.get(invalid..).unwrap_or_default();
                }
            }
        }
    }

    fn hold(&mut self, bytes: &[u8]) {
        let len = bytes.len().min(SEQUENCE_MAX);
        let (target, _) = self.pending.split_at_mut(len);
        let (source, _) = bytes.split_at(len);
        target.copy_from_slice(source);
        self.pending_len = len;
    }
}

impl Default for Utf8Assembler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::string::String;

    use super::Utf8Assembler;

    fn decoded(pieces: &[&[u8]]) -> String {
        let mut assembler = Utf8Assembler::new();
        let mut text = String::new();
        for piece in pieces {
            assembler.push(piece, |chunk| text.push_str(chunk));
        }
        assembler.finish(|chunk| text.push_str(chunk));
        text
    }

    #[test]
    fn a_character_split_between_pieces_is_joined() {
        let bytes = "ж€😀".as_bytes();
        for split in 0..=bytes.len() {
            let (head, tail) = bytes.split_at(split);
            assert_eq!(decoded(&[head, tail]), "ж€😀", "split at {split}");
        }
        let single: [&[u8]; 4] = [&[0xF0], &[0x9F], &[0x98], &[0x80]];
        assert_eq!(decoded(&single), "😀");
    }

    #[test]
    fn invalid_bytes_become_replacement_characters() {
        assert_eq!(decoded(&[b"a\xFFb"]), "a\u{FFFD}b");
        assert_eq!(
            decoded(&[b"\xE0\x80"]),
            String::from_utf8_lossy(b"\xE0\x80")
        );
    }

    #[test]
    fn a_sequence_broken_across_pieces_is_replaced_and_the_rest_kept() {
        assert_eq!(decoded(&[b"x\xE2\x82", b"y"]), "x\u{FFFD}y");
        assert_eq!(decoded(&[b"\xF0", b"\x9F", b"!"]), "\u{FFFD}!");
    }

    #[test]
    fn finish_replaces_an_unterminated_sequence() {
        assert_eq!(decoded(&[b"ok\xE2\x82"]), "ok\u{FFFD}");
    }
}
