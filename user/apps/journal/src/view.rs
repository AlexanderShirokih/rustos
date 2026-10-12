//! Раскладка записей журнала на строки сетки моноширинного шрифта.

use core::{iter, mem};
use std::collections::VecDeque;

use log_abi::{Entry, Level};

const VISIBLE_LINES_MAX: usize = 128;
const KERNEL_SOURCE: &str = "kernel";
const TIME_WIDTH: usize = 9;
const TIME_SECONDS_MODULUS: u64 = 100_000;
const NS_PER_SECOND: u64 = 1_000_000_000;
const NS_PER_MILLISECOND: u64 = 1_000_000;
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
    pub(crate) level: Level,
    pub(crate) kernel: bool,
    pub(crate) time: String,
    pub(crate) source: String,
    pub(crate) text: String,
    pub(crate) wrapped: bool,
}

#[derive(Debug)]
pub(crate) struct Pushed {
    pub(crate) evicted: usize,
    pub(crate) lines: Vec<VisualLine>,
}

#[derive(Debug)]
pub(crate) struct JournalView {
    columns: usize,
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
            columns,
            source_width,
            text_width: columns
                .saturating_sub(TIME_WIDTH + 1 + source_width + 1)
                .max(1),
            entries: VecDeque::new(),
        }
    }

    pub(crate) const fn source_column() -> usize {
        TIME_WIDTH + 1
    }

    pub(crate) fn text_column(&self) -> usize {
        Self::source_column() + self.source_width + 1
    }

    pub(crate) fn push(&mut self, entry: Entry<'_>) -> Pushed {
        match entry {
            Entry::Record { record, .. } => {
                let (source, text) = if record.kernel {
                    (KERNEL_SOURCE, format!("{}: {}", record.source, record.text))
                } else {
                    (record.source, String::from(record.text))
                };
                self.push_entry(
                    Head {
                        kind: LineKind::Line,
                        level: record.level,
                        kernel: record.kernel,
                        time: time_column(record.time_ns),
                        source: fit(source, self.source_width),
                    },
                    &text,
                )
            }
            Entry::Gap { lost: 1 } => self.push_entry(Head::marker(LineKind::Gap), "1 line lost"),
            Entry::Gap { lost } => {
                self.push_entry(Head::marker(LineKind::Gap), &format!("{lost} lines lost"))
            }
        }
    }

    pub(crate) fn push_failure(&mut self, reason: &str) -> Pushed {
        self.push_entry(Head::marker(LineKind::Failure), reason)
    }

    fn push_entry(&mut self, head: Head, text: &str) -> Pushed {
        let Head {
            kind,
            level,
            kernel,
            mut time,
            mut source,
        } = head;
        let lines: Vec<VisualLine> = wrap(text, self.text_width, self.columns.max(1))
            .into_iter()
            .enumerate()
            .map(|(index, text)| VisualLine {
                kind,
                level,
                kernel,
                time: mem::take(&mut time),
                source: mem::take(&mut source),
                text,
                wrapped: index > 0,
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

struct Head {
    kind: LineKind,
    level: Level,
    kernel: bool,
    time: String,
    source: String,
}

impl Head {
    fn marker(kind: LineKind) -> Self {
        Self {
            kind,
            level: Level::Info,
            kernel: false,
            time: String::new(),
            source: String::new(),
        }
    }
}

fn time_column(time_ns: u64) -> String {
    let seconds = time_ns / NS_PER_SECOND % TIME_SECONDS_MODULUS;
    let millis = time_ns % NS_PER_SECOND / NS_PER_MILLISECOND;
    format!("{seconds:5}.{millis:03}")
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

fn wrap(text: &str, first_width: usize, wrapped_width: usize) -> Vec<String> {
    let mut chars = text.chars().peekable();
    let mut pieces = Vec::new();
    let mut width = first_width;
    loop {
        pieces.push(chars.by_ref().take(width).collect());
        width = wrapped_width;
        if chars.peek().is_none() {
            return pieces;
        }
    }
}
