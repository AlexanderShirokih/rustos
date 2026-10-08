use std::{cell::OnceCell, collections::BTreeMap, path::PathBuf, sync::Arc};

use ra_ap_syntax::{
    AstNode, AstToken, Edition, SourceFile, SyntaxKind, SyntaxNode,
    ast::{self, HasAttrs},
};

use crate::{
    scope::{Domain, FileMeta},
    settings::Settings,
    span::{LineIndex, Span},
};

/// Токен комментария вместе с его положением и природой.
pub struct CommentTok {
    /// Диапазон токена.
    pub span: Span,
    /// Текст токена.
    pub text: String,
    /// Токен является doc-комментарием.
    pub is_doc: bool,
    /// Токен лежит внутри тела макроса.
    pub in_macro_body: bool,
}

/// Строковый литерал вместе с его положением.
pub struct LiteralTok {
    /// Диапазон токена.
    pub span: Span,
    /// Текст токена вместе с кавычками.
    pub text: String,
}

/// Результат разбора файла, вычисляемый один раз по требованию.
pub struct Parsed {
    /// Корень дерева.
    pub root: SyntaxNode,
    /// Узлы, сгруппированные по виду.
    pub index: BTreeMap<SyntaxKind, Vec<SyntaxNode>>,
    /// Комментарии в порядке возрастания смещения.
    pub comments: Vec<CommentTok>,
    /// Диапазоны тел макро-объявлений и макро-вызовов.
    pub macro_bodies: Vec<Span>,
    /// Строковые литералы, включая лежащие в телах макросов.
    pub literals: Vec<LiteralTok>,
    /// Диапазоны тестового контекста.
    pub test_ranges: Vec<Span>,
    /// Число ошибок разбора.
    pub errors: usize,
}

/// Файл вместе с ленивыми индексами по нему.
pub struct Source {
    /// Паспорт файла.
    pub meta: FileMeta,
    /// Текст файла, пригодный для разбора.
    pub text: String,
    /// Настройки, общие для прогона.
    pub settings: Arc<Settings>,
    parsed: OnceCell<Parsed>,
    lines: OnceCell<LineIndex>,
}

impl Source {
    /// Собирает файл из пути, текста и настроек.
    pub fn new(meta: FileMeta, text: &str, settings: Arc<Settings>) -> Source {
        Source {
            meta,
            text: sanitize(text),
            settings,
            parsed: OnceCell::new(),
            lines: OnceCell::new(),
        }
    }

    /// Собирает файл вне репозитория, для тестов и фикстур.
    pub fn detached(path: impl Into<PathBuf>, text: &str) -> Source {
        let path = path.into();
        Source::new(
            FileMeta {
                domain: Domain::of(&path),
                path,
                package: None,
            },
            text,
            Arc::new(Settings::default()),
        )
    }

    /// Возвращает разбор, выполняя его при первом обращении.
    pub fn parsed(&self) -> &Parsed {
        self.parsed
            .get_or_init(|| parse(&self.text, &self.settings, &self.meta))
    }

    /// Возвращает индекс строк, строя его при первом обращении.
    pub fn lines(&self) -> &LineIndex {
        self.lines.get_or_init(|| LineIndex::new(&self.text))
    }

    /// Сообщает, лежит ли диапазон в тестовом контексте.
    pub fn is_test(&self, span: Span) -> bool {
        self.parsed()
            .test_ranges
            .iter()
            .any(|range| range.contains(span))
    }

    /// Сообщает, лежит ли диапазон внутри тела макроса.
    pub fn in_macro_body(&self, span: Span) -> bool {
        self.parsed()
            .macro_bodies
            .iter()
            .any(|body| body.contains(span))
    }
}

fn sanitize(text: &str) -> String {
    match text.strip_prefix('\u{feff}') {
        Some(rest) => format!("   {rest}"),
        None => text.to_owned(),
    }
}

fn parse(text: &str, settings: &Settings, meta: &FileMeta) -> Parsed {
    let parse = SourceFile::parse(text, Edition::CURRENT);
    let errors = parse.errors().len();
    let root = parse.syntax_node();

    let mut index: BTreeMap<SyntaxKind, Vec<SyntaxNode>> = BTreeMap::new();
    let mut macro_bodies: Vec<Span> = Vec::new();
    let mut test_ranges: Vec<Span> = Vec::new();

    if file_is_test(meta) {
        test_ranges.push(Span::new(0, crate::span::offset(text.len())));
    }

    for node in root.descendants() {
        index.entry(node.kind()).or_default().push(node.clone());
        if node.kind() == SyntaxKind::TOKEN_TREE
            && node.parent().is_some_and(|parent| {
                matches!(
                    parent.kind(),
                    SyntaxKind::MACRO_CALL | SyntaxKind::MACRO_RULES | SyntaxKind::MACRO_DEF
                )
            })
        {
            macro_bodies.push(span_of(&node));
        }
        if declares_test(&node, settings) {
            test_ranges.push(span_of(&node));
        }
    }

    let mut comments: Vec<CommentTok> = Vec::new();
    let mut literals: Vec<LiteralTok> = Vec::new();
    for element in root.descendants_with_tokens() {
        let Some(token) = element.as_token() else {
            continue;
        };
        let range = token.text_range();
        let span = Span::new(range.start().into(), range.end().into());
        match token.kind() {
            SyntaxKind::COMMENT => {
                let is_doc = ast::Comment::cast(token.clone())
                    .is_some_and(|comment| comment.kind().doc.is_some());
                let in_macro_body = macro_bodies.iter().any(|body| body.contains(span));
                comments.push(CommentTok {
                    span,
                    text: token.text().to_owned(),
                    is_doc,
                    in_macro_body,
                });
            }
            SyntaxKind::STRING | SyntaxKind::BYTE_STRING | SyntaxKind::C_STRING => {
                literals.push(LiteralTok {
                    span,
                    text: token.text().to_owned(),
                });
            }
            _ => {}
        }
    }
    comments.sort_by_key(|comment| comment.span.start);
    literals.sort_by_key(|literal| literal.span.start);

    Parsed {
        root,
        index,
        comments,
        macro_bodies,
        literals,
        test_ranges,
        errors,
    }
}

fn span_of(node: &SyntaxNode) -> Span {
    let range = node.text_range();
    Span::new(range.start().into(), range.end().into())
}

fn file_is_test(meta: &FileMeta) -> bool {
    let path = meta.path.to_string_lossy();
    path.contains("/tests/") || path.contains("/kernel_tests/")
}

fn declares_test(node: &SyntaxNode, settings: &Settings) -> bool {
    let Some(owner) = ast::AnyHasAttrs::cast(node.clone()) else {
        return false;
    };
    owner.attrs().any(|attr| {
        let text = attr.syntax().text().to_string();
        let name = attribute_head(&text);
        if settings.test_attributes.contains(&name) {
            return true;
        }
        name == "cfg" && names_test(&drop_negations(&text))
    })
}

fn attribute_head(text: &str) -> String {
    text.trim_start_matches(['#', '!', '['])
        .trim_start()
        .chars()
        .take_while(|ch| ch.is_alphanumeric() || matches!(ch, '_' | ':'))
        .collect()
}

fn drop_negations(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("not(") {
        out.push_str(&rest[..at]);
        let tail = &rest[at + "not(".len()..];
        let mut depth = 1usize;
        let mut cut = tail.len();
        for (offset, ch) in tail.char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {}
            }
            if depth == 0 {
                cut = offset + 1;
                break;
            }
        }
        rest = &tail[cut..];
    }
    out.push_str(rest);
    out
}

fn names_test(text: &str) -> bool {
    text.match_indices("test").any(|(at, word)| {
        let before = text[..at].chars().next_back();
        let after = text[at + word.len()..].chars().next();
        !before.is_some_and(is_word) && !after.is_some_and(is_word)
    })
}

fn is_word(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}
