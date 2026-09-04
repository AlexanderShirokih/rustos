use ra_ap_syntax::{
    AstNode, SyntaxKind, SyntaxNode, TextRange, TextSize,
    ast::{self, HasName},
};

use crate::{source::Source, span::Span};

/// Обозначение объявления, накрывающего находку, для файла вне дерева разбора.
pub const FILE_ROOT: &str = "<file-root>";

/// Возвращает путь объявления, накрывающего диапазон.
pub fn item_path(source: &Source, span: Span) -> String {
    let root = &source.parsed().root;
    let end = span.end.min(crate::span::offset(source.text.len()));
    let start = span.start.min(end);
    let range = TextRange::new(TextSize::from(start), TextSize::from(end));
    let Some(element) = root.covering_element(range).into_node().or_else(|| {
        root.covering_element(range)
            .into_token()
            .and_then(|token| token.parent())
    }) else {
        return FILE_ROOT.to_owned();
    };

    let mut segments: Vec<String> = Vec::new();
    for node in element.ancestors() {
        if let Some(segment) = segment(&node) {
            segments.push(segment);
        }
    }
    if segments.is_empty() {
        return FILE_ROOT.to_owned();
    }
    segments.reverse();
    segments.join("::")
}

fn segment(node: &SyntaxNode) -> Option<String> {
    match node.kind() {
        SyntaxKind::IMPL => Some(impl_segment(node)),
        SyntaxKind::FN
        | SyntaxKind::STRUCT
        | SyntaxKind::ENUM
        | SyntaxKind::UNION
        | SyntaxKind::TRAIT
        | SyntaxKind::MODULE
        | SyntaxKind::CONST
        | SyntaxKind::STATIC
        | SyntaxKind::TYPE_ALIAS
        | SyntaxKind::MACRO_RULES
        | SyntaxKind::VARIANT
        | SyntaxKind::RECORD_FIELD => named(node),
        _ => None,
    }
}

fn named(node: &SyntaxNode) -> Option<String> {
    ast::AnyHasName::cast(node.clone())
        .and_then(|owner| owner.name())
        .map(|name| name.text().to_string())
}

fn impl_segment(node: &SyntaxNode) -> String {
    let Some(block) = ast::Impl::cast(node.clone()) else {
        return "<impl>".to_owned();
    };
    let target = block.self_ty().map_or_else(
        || "<self>".to_owned(),
        |ty| compact(&ty.syntax().text().to_string()),
    );
    match block.trait_() {
        Some(trait_ref) => {
            let name = compact(&trait_ref.syntax().text().to_string());
            format!("<{target} as {name}>")
        }
        None => target,
    }
}

fn compact(text: &str) -> String {
    crate::text::normalize(text)
}
