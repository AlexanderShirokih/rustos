use ra_ap_syntax::{SyntaxKind, SyntaxNode, TextRange, TextSize};

use crate::{rules::declaration::doc_block, source::Source, span::Span};

pub(crate) struct Zone {
    /// Правила, на которые директива распространяется.
    pub rules: Vec<String>,
    /// Диапазон, находки внутри которого директива снимает.
    pub covers: Span,
    /// Диапазон самой директивы, находки внутри которого снимаются для всех правил.
    pub itself: Span,
    /// Обоснование указано.
    pub has_reason: bool,
    /// Директива применилась хотя бы к одной находке.
    pub used: bool,
}

pub(crate) struct Suppressions {
    /// Зоны в порядке появления.
    pub zones: Vec<Zone>,
}

const INLINE: &str = "style:allow";
const WHOLE_FILE: &str = "style:allow-file";

impl Suppressions {
    pub(crate) fn collect(source: &Source) -> Suppressions {
        if source.meta.path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            return Suppressions { zones: Vec::new() };
        }
        let parsed = source.parsed();
        let lines = source.lines();
        let end = crate::span::offset(source.text.len());
        let mut zones = Vec::new();

        for comment in &parsed.comments {
            let body = comment.text.trim_start_matches('/').trim_start();
            let Some((marker, whole_file)) = marker_of(&comment.text) else {
                continue;
            };
            let rest = body.get(marker.len()..).unwrap_or_default();
            let Some((rules, reason)) = split_directive(rest) else {
                continue;
            };

            let line = lines.line_of(comment.span.start);
            let line_span = lines.line_span(&source.text, line);
            let head = source
                .text
                .get(line_span.start as usize..comment.span.start as usize)
                .unwrap_or_default();
            let standalone = head.trim().is_empty();

            let covers = if whole_file {
                Span::new(0, end)
            } else if standalone {
                covered_below(source, line)
            } else {
                line_span
            };
            let itself = if standalone { line_span } else { comment.span };

            zones.push(Zone {
                rules,
                covers,
                itself,
                has_reason: !reason.trim().is_empty(),
                used: false,
            });
        }

        Suppressions { zones }
    }

    pub(crate) fn is_directive_text(&self, at: Span) -> bool {
        self.zones.iter().any(|zone| zone.itself.contains(at))
    }

    pub(crate) fn covers(&mut self, rule: &str, at: Span) -> bool {
        let mut hit = false;
        for zone in &mut self.zones {
            if zone.covers.contains(at) && zone.rules.iter().any(|want| want == rule) {
                zone.used = true;
                hit = true;
            }
        }
        hit
    }
}

pub(crate) fn is_directive(text: &str) -> bool {
    marker_of(text).is_some()
}

fn marker_of(text: &str) -> Option<(&'static str, bool)> {
    let body = text.trim_start_matches('/').trim_start();
    if body.starts_with(WHOLE_FILE) {
        return Some((WHOLE_FILE, true));
    }
    if body.starts_with(INLINE) {
        return Some((INLINE, false));
    }
    None
}

fn split_directive(rest: &str) -> Option<(Vec<String>, String)> {
    let rest = rest.trim_start();
    let inner = rest.strip_prefix('(')?;
    let close = inner.find(')')?;
    let rules = inner
        .get(..close)?
        .split(',')
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .collect::<Vec<String>>();
    if rules.is_empty() {
        return None;
    }
    Some((rules, inner.get(close + 1..).unwrap_or_default().to_owned()))
}

fn covered_below(source: &Source, line: usize) -> Span {
    let lines = source.lines();
    let mut target = line + 1;
    while target < lines.line_count() {
        let span = lines.line_span(&source.text, target);
        let text = source
            .text
            .get(span.start as usize..span.end as usize)
            .unwrap_or_default();
        if !text.trim().is_empty() {
            return declaration_span(source, span).unwrap_or(span);
        }
        target += 1;
    }
    lines.line_span(&source.text, line)
}

fn declaration_span(source: &Source, line: Span) -> Option<Span> {
    let root = &source.parsed().root;
    let range = TextRange::new(TextSize::from(line.start), TextSize::from(line.end));
    let element = root.covering_element(range);
    let node = element
        .clone()
        .into_node()
        .or_else(|| element.into_token().and_then(|token| token.parent()))?;
    let owner = node.ancestors().find(is_declaration)?;
    let range = owner.text_range();
    let start = doc_block(&owner).map_or_else(|| range.start().into(), |block| block.span.start);
    Some(Span::new(start, range.end().into()))
}

fn is_declaration(node: &SyntaxNode) -> bool {
    matches!(
        node.kind(),
        SyntaxKind::CONST
            | SyntaxKind::ENUM
            | SyntaxKind::FN
            | SyntaxKind::MACRO_RULES
            | SyntaxKind::MODULE
            | SyntaxKind::RECORD_FIELD
            | SyntaxKind::STATIC
            | SyntaxKind::STRUCT
            | SyntaxKind::TRAIT
            | SyntaxKind::TUPLE_FIELD
            | SyntaxKind::TYPE_ALIAS
            | SyntaxKind::UNION
            | SyntaxKind::USE
            | SyntaxKind::VARIANT
    )
}
