//! Сборка строк журнала из фрагментов потока вывода.

use crate::LogBuffer;

/// Накопитель строк. Завершённые уходят наружу, незавершённый остаток копится.
pub struct LineAssembler<const N: usize> {
    buffer: LogBuffer<N>,
}

impl<const N: usize> LineAssembler<N> {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buffer: LogBuffer::new(),
        }
    }

    /// Разбирает `text` на строки. Каждая завершённая уходит в `emit`, включая пустую.
    /// Строка длиннее `N` уходит несколькими частями.
    pub fn push(&mut self, text: &str, mut emit: impl FnMut(&str)) {
        let mut rest = text;
        while let Some(end) = rest.find('\n') {
            let (line, tail) = rest.split_at(end);
            self.fill(line, &mut emit);
            emit(self.buffer.as_str());
            self.buffer.clear();
            rest = &tail['\n'.len_utf8()..];
        }
        self.fill(rest, &mut emit);
    }

    /// Отдаёт незавершённый остаток. Пустой накопитель записи не создаёт.
    pub fn flush(&mut self, mut emit: impl FnMut(&str)) {
        if !self.buffer.is_empty() {
            emit(self.buffer.as_str());
            self.buffer.clear();
        }
    }

    fn fill(&mut self, text: &str, emit: &mut impl FnMut(&str)) {
        let mut rest = text;
        while !rest.is_empty() {
            let taken = self.buffer.push_str(rest);
            if taken == 0 && self.buffer.is_empty() {
                // Символ не помещается даже в пустой накопитель. Кадр его не содержит.
                return;
            }
            rest = &rest[taken..];
            if rest.is_empty() {
                return;
            }
            emit(self.buffer.as_str());
            self.buffer.clear();
        }
    }
}

impl<const N: usize> Default for LineAssembler<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::{string::String, vec::Vec};

    use super::LineAssembler;

    fn assembled<const N: usize>(fragments: &[&str]) -> Vec<String> {
        let mut assembler = LineAssembler::<N>::new();
        let mut lines = Vec::new();
        for fragment in fragments {
            assembler.push(fragment, |line| lines.push(String::from(line)));
        }
        lines
    }

    #[test]
    fn fragments_of_one_line_join_into_one_record() {
        // Так `core::fmt` разбирает `println!("rtc reads {seconds}")`.
        assert_eq!(
            assembled::<64>(&["rtc reads ", "17", "\n"]),
            ["rtc reads 17"]
        );
    }

    #[test]
    fn a_fragment_with_several_lines_splits_into_records() {
        assert_eq!(assembled::<64>(&["first\nsecond\n"]), ["first", "second"]);
    }

    #[test]
    fn an_empty_line_stays_a_record() {
        assert_eq!(assembled::<64>(&["\n"]), [""]);
    }

    #[test]
    fn an_unterminated_tail_waits_for_its_line() {
        assert_eq!(assembled::<64>(&["tail"]), Vec::<String>::new());
    }

    #[test]
    fn flush_gives_out_the_unterminated_tail() {
        let mut assembler = LineAssembler::<64>::new();
        let mut lines = Vec::new();
        assembler.push("tail", |line| lines.push(String::from(line)));
        assembler.flush(|line| lines.push(String::from(line)));
        assembler.flush(|line| lines.push(String::from(line)));
        assert_eq!(lines, ["tail"]);
    }

    #[test]
    fn a_line_beyond_the_bound_goes_in_parts() {
        let lines = assembled::<4>(&["abcdefg\n"]);
        assert_eq!(lines, ["abcd", "efg"]);
    }

    #[test]
    fn parts_of_a_long_line_split_on_a_char_boundary() {
        // Символы двухбайтовые, граница накопителя приходится на середину одного из них.
        let lines = assembled::<3>(&["éèê\n"]);
        assert_eq!(lines.concat(), "éèê");
        assert!(lines.iter().all(|line| !line.is_empty()));
    }
}
