use crate::{
    rules::declaration::{DECLARATIONS, opens_safety_contract, plain_block},
    source::Source,
    span::Span,
    view::{CommentView, SyntaxView, View},
};

/// Строка текста, описывающего объявление.
pub struct ProseLine {
    /// Диапазон токена комментария.
    pub span: Span,
    /// Текст токена вместе с маркером комментария.
    pub text: String,
}

/// Собирает текст объявлений файла, сгруппированный в блоки соседних строк.
pub fn blocks(source: &Source) -> Vec<Vec<ProseLine>> {
    let Some(view) = CommentView::of(source) else {
        return Vec::new();
    };
    let attached = plain_spans(source);
    let index = source.lines();

    let mut out: Vec<Vec<ProseLine>> = Vec::new();
    let mut previous: Option<usize> = None;
    for comment in view.comments {
        if !(comment.is_doc || attached.iter().any(|block| block.contains(comment.span))) {
            previous = None;
            continue;
        }
        let line = index.line_of(comment.span.start);
        if previous.is_none_or(|last| line != last + 1) {
            out.push(Vec::new());
        }
        if let Some(block) = out.last_mut() {
            block.push(ProseLine {
                span: comment.span,
                text: comment.text.clone(),
            });
        }
        previous = Some(line);
    }
    out
}

/// Отделяет маркер комментария от тела, сохраняя отступ тела.
pub fn body_of(text: &str) -> (usize, &str) {
    let bare = text.trim_start_matches('/');
    let body = bare.strip_prefix('!').unwrap_or(bare);
    (text.len() - body.len(), body)
}

fn plain_spans(source: &Source) -> Vec<Span> {
    let Some(view) = SyntaxView::of(source) else {
        return Vec::new();
    };
    let mut spans = Vec::new();
    for kind in DECLARATIONS {
        for node in view.nodes(*kind) {
            if opens_safety_contract(&node) {
                continue;
            }
            if let Some(block) = plain_block(&node) {
                spans.push(block.span);
            }
        }
    }
    spans
}
