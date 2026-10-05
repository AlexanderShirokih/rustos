use std::{mem, ops::Range};

use crate::{
    rule::{Finding, Strip, TextRule, TextTarget},
    rules::attached,
    source::{CommentTok, Source},
    span::Span,
    text::mask_backticks,
    view::{CommentView, LineView, View},
};

pub(crate) fn run(rule: &TextRule, source: &Source, out: &mut Vec<Finding>) {
    for unit in units(rule.target, source) {
        let subject = strip(&unit.text, &rule.strip);
        if !rule.require.iter().all(|need| need.is_match(&subject)) {
            continue;
        }
        for hit in rule.pattern.find_iter(&subject) {
            let sentence = &subject[sentence_around(&subject, hit.end().saturating_sub(1))];
            if rule.unless.iter().any(|veto| veto.is_match(sentence)) {
                continue;
            }
            let start = skip_boundary(&subject, hit.start(), hit.end());
            out.push(Finding::new(
                unit.map
                    .translate(crate::span::offset(start), crate::span::offset(hit.end())),
                rule.message.clone(),
            ));
        }
    }
}

fn sentence_around(text: &str, at: usize) -> Range<usize> {
    let bytes = text.as_bytes();
    let mut start = 0;
    for index in 0..at.min(bytes.len()) {
        if ends_sentence(bytes, index) {
            start = index + 1;
        }
    }
    while bytes.get(start).is_some_and(u8::is_ascii_whitespace) {
        start += 1;
    }
    let mut end = at.min(bytes.len());
    while end < bytes.len() && !ends_sentence(bytes, end) {
        end += 1;
    }
    start..(end + 1).min(bytes.len()).max(start)
}

fn ends_sentence(bytes: &[u8], index: usize) -> bool {
    bytes
        .get(index)
        .is_some_and(|byte| matches!(byte, b'.' | b'!' | b'?'))
        && bytes.get(index + 1).is_none_or(u8::is_ascii_whitespace)
}

fn skip_boundary(text: &str, start: usize, end: usize) -> usize {
    let bytes = text.as_bytes();
    let mut at = start;
    while at < end
        && bytes
            .get(at)
            .is_some_and(|byte| matches!(byte, b'.' | b'!' | b'?' | b' ' | b'\t'))
    {
        at += 1;
    }
    at
}

struct Piece {
    joined_start: u32,
    joined_end: u32,
    file_start: u32,
}

struct OffsetMap {
    pieces: Vec<Piece>,
}

impl OffsetMap {
    fn single(start: u32, len: u32) -> OffsetMap {
        OffsetMap {
            pieces: vec![Piece {
                joined_start: 0,
                joined_end: len,
                file_start: start,
            }],
        }
    }

    fn translate(&self, start: u32, end: u32) -> Span {
        let file_start = self.locate(start);
        let file_end = self.locate(end.saturating_sub(1).max(start)) + 1;
        Span::new(file_start, file_end)
    }

    fn locate(&self, at: u32) -> u32 {
        let piece = self
            .pieces
            .iter()
            .rev()
            .find(|piece| piece.joined_start <= at)
            .or_else(|| self.pieces.first());
        let Some(piece) = piece else {
            return 0;
        };
        let span = piece.joined_end - piece.joined_start;
        piece.file_start + at.saturating_sub(piece.joined_start).min(span)
    }
}

struct Unit {
    text: String,
    map: OffsetMap,
}

#[derive(Default)]
struct Joiner {
    text: String,
    pieces: Vec<Piece>,
}

impl Joiner {
    fn push(&mut self, file_start: u32, content: &str) {
        if !self.pieces.is_empty() {
            self.text.push(' ');
        }
        let joined_start = crate::span::offset(self.text.len());
        self.text.push_str(content);
        self.pieces.push(Piece {
            joined_start,
            joined_end: crate::span::offset(self.text.len()),
            file_start,
        });
    }

    fn take(&mut self) -> Option<Unit> {
        if self.pieces.is_empty() {
            return None;
        }
        Some(Unit {
            text: mem::take(&mut self.text),
            map: OffsetMap {
                pieces: mem::take(&mut self.pieces),
            },
        })
    }
}

fn units(target: TextTarget, source: &Source) -> Vec<Unit> {
    match target {
        TextTarget::Attached => attached::blocks(source)
            .into_iter()
            .flatten()
            .map(|line| {
                let len = crate::span::offset(line.text.len());
                Unit {
                    map: OffsetMap::single(line.span.start, len),
                    text: line.text,
                }
            })
            .collect(),
        TextTarget::Comment | TextTarget::Doc => {
            let Some(view) = CommentView::of(source) else {
                return Vec::new();
            };
            view.comments
                .iter()
                .filter(|comment| target != TextTarget::Doc || comment.is_doc)
                .map(|comment| {
                    let len = crate::span::offset(comment.text.len());
                    Unit {
                        map: OffsetMap::single(comment.span.start, len),
                        text: comment.text.clone(),
                    }
                })
                .collect()
        }
        TextTarget::Line => {
            let Some(view) = LineView::of(source) else {
                return Vec::new();
            };
            (0..view.count())
                .map(|number| {
                    let text = view.text_of(number).to_owned();
                    let len = crate::span::offset(text.len());
                    Unit {
                        map: OffsetMap::single(view.span(number).start, len),
                        text,
                    }
                })
                .collect()
        }
        TextTarget::Paragraph => {
            if is_rust(source) {
                rust_paragraphs(source)
            } else {
                text_paragraphs(source)
            }
        }
    }
}

fn is_rust(source: &Source) -> bool {
    source.meta.path.extension().and_then(|ext| ext.to_str()) == Some("rs")
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CommentKind {
    Outer,
    Inner,
    Plain,
}

impl CommentKind {
    fn of(comment: &CommentTok) -> CommentKind {
        if !comment.is_doc {
            CommentKind::Plain
        } else if comment.text.starts_with("//!") {
            CommentKind::Inner
        } else {
            CommentKind::Outer
        }
    }
}

fn rust_paragraphs(source: &Source) -> Vec<Unit> {
    let Some(view) = CommentView::of(source) else {
        return Vec::new();
    };
    let index = source.lines();
    let mut out = Vec::new();
    let mut joiner = Joiner::default();
    let mut previous: Option<(usize, CommentKind)> = None;

    for comment in view.comments {
        let kind = CommentKind::of(comment);
        let line = index.line_of(comment.span.start);
        let (marker, body) = attached::body_of(&comment.text);
        let trimmed = body.trim();

        if trimmed.is_empty() {
            if let Some(unit) = joiner.take() {
                out.push(unit);
            }
            previous = None;
            continue;
        }

        let continues = previous
            .is_some_and(|(last_line, last_kind)| last_kind == kind && line == last_line + 1);
        if !continues && let Some(unit) = joiner.take() {
            out.push(unit);
        }

        let leading = body.len() - body.trim_start().len();
        let content_start = comment.span.start + crate::span::offset(marker + leading);
        joiner.push(content_start, trimmed);
        previous = Some((line, kind));
    }
    if let Some(unit) = joiner.take() {
        out.push(unit);
    }
    out
}

fn text_paragraphs(source: &Source) -> Vec<Unit> {
    let Some(view) = LineView::of(source) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut joiner = Joiner::default();
    let mut fenced = false;

    for number in 0..view.count() {
        let text = view.text_of(number);
        let trimmed = text.trim();

        if trimmed.starts_with("```") {
            fenced = !fenced;
            if let Some(unit) = joiner.take() {
                out.push(unit);
            }
            continue;
        }
        if fenced || trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('|') {
            if let Some(unit) = joiner.take() {
                out.push(unit);
            }
            continue;
        }

        let leading = crate::span::offset(text.len() - text.trim_start().len());
        let content_start = view.span(number).start + leading;
        joiner.push(content_start, trimmed);
    }
    if let Some(unit) = joiner.take() {
        out.push(unit);
    }
    out
}

fn strip(text: &str, steps: &[Strip]) -> String {
    let mut current = text.to_owned();
    for step in steps {
        current = match step {
            Strip::Backticks => mask_backticks(&current),
        };
    }
    current
}
