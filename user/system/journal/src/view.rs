//! Раскладка записей журнала по сетке моноширинного шрифта: источник в колонке слева, текст
//! после неё с переносом под начало текста. Модель хранит последние `VISIBLE_LINES_MAX`
//! записей и сообщает, сколько визуальных строк удалено с вытесненной записью.

use core::iter;
use std::collections::VecDeque;

use log_abi::JournalEntry;

const VISIBLE_LINES_MAX: usize = 128;
const SOURCE_WIDE: usize = 18;
const SOURCE_NARROW: usize = 12;
const SOURCE_WIDE_COLUMNS_MIN: usize = 80;
const SOURCE_ELLIPSIS: char = '…';

#[derive(Debug, Clone, Copy)]
pub(crate) enum LineKind {
    Line,
    Gap,
    Failure,
}

#[derive(Debug)]
pub(crate) struct VisualLine {
    pub(crate) kind: LineKind,
    pub(crate) source: String,
    pub(crate) text: String,
}

#[derive(Debug)]
pub(crate) struct Pushed {
    pub(crate) evicted: usize,
    pub(crate) lines: Vec<VisualLine>,
}

#[derive(Debug)]
pub(crate) struct JournalView {
    source_width: usize,
    text_width: usize,
    entries: VecDeque<usize>,
}

impl JournalView {
    pub(crate) fn new(columns: usize) -> Self {
        let source_width = if columns >= SOURCE_WIDE_COLUMNS_MIN {
            SOURCE_WIDE
        } else {
            SOURCE_NARROW
        };
        Self {
            source_width,
            text_width: columns.saturating_sub(source_width + 1).max(1),
            entries: VecDeque::new(),
        }
    }

    pub(crate) fn text_column(&self) -> usize {
        self.source_width + 1
    }

    pub(crate) fn push(&mut self, entry: JournalEntry<'_>) -> Pushed {
        match entry {
            JournalEntry::Line { source, text, .. } => {
                self.push_entry(LineKind::Line, source, text)
            }
            JournalEntry::Gap { lost: 1 } => self.push_entry(LineKind::Gap, "", "1 line lost"),
            JournalEntry::Gap { lost } => {
                self.push_entry(LineKind::Gap, "", &format!("{lost} lines lost"))
            }
        }
    }

    pub(crate) fn push_failure(&mut self, reason: &str) -> Pushed {
        self.push_entry(LineKind::Failure, "", reason)
    }

    fn push_entry(&mut self, kind: LineKind, source: &str, text: &str) -> Pushed {
        let mut source = Some(fit(source, self.source_width));
        let lines: Vec<VisualLine> = wrap(text, self.text_width)
            .into_iter()
            .map(|text| VisualLine {
                kind,
                source: source.take().unwrap_or_default(),
                text,
            })
            .collect();
        let evicted = if self.entries.len() < VISIBLE_LINES_MAX {
            0
        } else {
            self.entries.pop_front().unwrap_or(0)
        };
        self.entries.push_back(lines.len());
        Pushed { evicted, lines }
    }
}

fn fit(source: &str, width: usize) -> String {
    if source.chars().count() <= width {
        return source.into();
    }
    source
        .chars()
        .take(width.saturating_sub(1))
        .chain(iter::once(SOURCE_ELLIPSIS))
        .collect()
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut chars = text.chars().peekable();
    let mut pieces = Vec::new();
    loop {
        pieces.push(chars.by_ref().take(width).collect());
        if chars.peek().is_none() {
            return pieces;
        }
    }
}
