use ra_ap_syntax::{
    AstNode, AstToken, NodeOrToken, SyntaxKind, SyntaxNode,
    ast::{self, HasAttrs, HasDocComments, HasVisibility},
};

use crate::{settings::Settings, span::Span, suppress::is_directive};

pub const DECLARATIONS: &[SyntaxKind] = &[
    SyntaxKind::CONST,
    SyntaxKind::ENUM,
    SyntaxKind::FN,
    SyntaxKind::MACRO_RULES,
    SyntaxKind::MODULE,
    SyntaxKind::RECORD_FIELD,
    SyntaxKind::STATIC,
    SyntaxKind::STRUCT,
    SyntaxKind::TRAIT,
    SyntaxKind::TUPLE_FIELD,
    SyntaxKind::TYPE_ALIAS,
    SyntaxKind::UNION,
    SyntaxKind::USE,
    SyntaxKind::VARIANT,
];

pub struct CommentBlock {
    pub span: Span,
    pub lines: usize,
}

pub fn doc_block(node: &SyntaxNode) -> Option<CommentBlock> {
    let owner = ast::AnyHasDocComments::cast(node.clone())?;
    let mut comments: Vec<ast::Comment> = owner.doc_comments().collect();
    if let Some(inner) = owner.inner_attributes_node() {
        comments.extend(ast::DocCommentIter::from_syntax_node(&inner));
    }
    block(&comments)
}

pub fn plain_block(node: &SyntaxNode) -> Option<CommentBlock> {
    block(&leading_plain_comments(node))
}

pub fn leading_block(node: &SyntaxNode) -> Option<CommentBlock> {
    match (doc_block(node), plain_block(node)) {
        (Some(doc), Some(plain)) => Some(CommentBlock {
            span: Span::new(
                doc.span.start.min(plain.span.start),
                doc.span.end.max(plain.span.end),
            ),
            lines: doc.lines + plain.lines,
        }),
        (doc, plain) => doc.or(plain),
    }
}

pub fn opens_safety_contract(node: &SyntaxNode) -> bool {
    leading_plain_comments(node)
        .first()
        .is_some_and(|comment| body_of(comment).starts_with("SAFETY:"))
}

fn block(comments: &[ast::Comment]) -> Option<CommentBlock> {
    let first = comments.first()?;
    let last = comments.last()?;
    let lines = comments
        .iter()
        .map(|comment| comment.syntax().text().to_string().lines().count().max(1))
        .sum();
    Some(CommentBlock {
        span: Span::new(
            first.syntax().text_range().start().into(),
            last.syntax().text_range().end().into(),
        ),
        lines,
    })
}

fn leading_plain_comments(node: &SyntaxNode) -> Vec<ast::Comment> {
    let mut comments = Vec::new();
    for element in node.children_with_tokens() {
        match element {
            NodeOrToken::Token(token) if token.kind() == SyntaxKind::WHITESPACE => {}
            NodeOrToken::Token(token) if token.kind() == SyntaxKind::COMMENT => {
                let Some(comment) = ast::Comment::cast(token) else {
                    continue;
                };
                if comment.kind().doc.is_none() && !is_directive(comment.text()) {
                    comments.push(comment);
                }
            }
            NodeOrToken::Node(child) if child.kind() == SyntaxKind::ATTR => {}
            _ => break,
        }
    }
    comments
}

fn body_of(comment: &ast::Comment) -> &str {
    comment.text().trim_start_matches('/').trim_start()
}

pub fn carries_test_attribute(node: &SyntaxNode, settings: &Settings) -> bool {
    attribute_names(node).any(|name| settings.test_attributes.contains(&name))
}

pub fn permits_doc(node: &SyntaxNode, settings: &Settings) -> bool {
    if bears_generated_surface(node, settings) {
        return true;
    }
    if node.kind() == SyntaxKind::MACRO_RULES {
        return attribute_names(node).any(|name| name == "macro_export");
    }
    if node.kind() == SyntaxKind::VARIANT {
        return enclosing(node).is_some_and(|owner| is_plain_pub(&owner));
    }
    if let Some(owner) = enclosing(node)
        && is_associated(node)
    {
        if owner.kind() == SyntaxKind::TRAIT {
            return is_plain_pub(&owner);
        }
        if ast::Impl::cast(owner).is_some_and(|it| it.trait_().is_some()) {
            return false;
        }
    }
    is_plain_pub(node)
}

pub fn word(kind: SyntaxKind) -> &'static str {
    match kind {
        SyntaxKind::FN => "a function",
        SyntaxKind::STRUCT => "a struct",
        SyntaxKind::ENUM => "an enum",
        SyntaxKind::VARIANT => "an enum variant",
        SyntaxKind::TRAIT => "a trait",
        SyntaxKind::MODULE => "a module",
        SyntaxKind::CONST => "a constant",
        SyntaxKind::STATIC => "a static",
        SyntaxKind::TYPE_ALIAS => "a type alias",
        SyntaxKind::RECORD_FIELD | SyntaxKind::TUPLE_FIELD => "a field",
        SyntaxKind::UNION => "a union",
        SyntaxKind::USE => "a use declaration",
        SyntaxKind::MACRO_RULES => "a macro",
        _ => "a declaration",
    }
}

fn attribute_names(node: &SyntaxNode) -> impl Iterator<Item = String> {
    ast::AnyHasAttrs::cast(node.clone())
        .into_iter()
        .flat_map(|owner| owner.attrs())
        .filter_map(|attr| attr.path().map(|path| path.syntax().text().to_string()))
        .map(|name| name.trim().to_owned())
}

fn bears_generated_surface(node: &SyntaxNode, settings: &Settings) -> bool {
    let carrier = match node.kind() {
        SyntaxKind::RECORD_FIELD | SyntaxKind::TUPLE_FIELD | SyntaxKind::VARIANT => {
            owning_type(node)
        }
        _ => Some(node.clone()),
    };
    carrier.is_some_and(|owner| has_doc_bearing_derive(&owner, settings))
}

fn has_doc_bearing_derive(node: &SyntaxNode, settings: &Settings) -> bool {
    let Some(owner) = ast::AnyHasAttrs::cast(node.clone()) else {
        return false;
    };
    owner.attrs().any(|attr| {
        let is_derive = attr
            .path()
            .is_some_and(|path| path.syntax().text().to_string().trim() == "derive");
        if !is_derive {
            return false;
        }
        let listed = attr.syntax().text().to_string();
        settings
            .doc_bearing_derives
            .iter()
            .any(|want| listed.contains(want.as_str()))
    })
}

fn owning_type(node: &SyntaxNode) -> Option<SyntaxNode> {
    node.ancestors().skip(1).find(|owner| {
        matches!(
            owner.kind(),
            SyntaxKind::STRUCT | SyntaxKind::ENUM | SyntaxKind::UNION
        )
    })
}

fn enclosing(node: &SyntaxNode) -> Option<SyntaxNode> {
    node.ancestors().skip(1).find(|owner| {
        matches!(
            owner.kind(),
            SyntaxKind::TRAIT | SyntaxKind::IMPL | SyntaxKind::ENUM
        )
    })
}

fn is_associated(node: &SyntaxNode) -> bool {
    node.parent()
        .is_some_and(|parent| parent.kind() == SyntaxKind::ASSOC_ITEM_LIST)
}

fn is_plain_pub(node: &SyntaxNode) -> bool {
    ast::AnyHasVisibility::cast(node.clone())
        .and_then(|owner| owner.visibility())
        .is_some_and(|vis| vis.syntax().text().to_string().trim() == "pub")
}
