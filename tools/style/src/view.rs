use std::collections::BTreeMap;

use ra_ap_syntax::{SyntaxKind, SyntaxNode};

use crate::{
    settings::Settings,
    source::{CommentTok, LiteralTok, Source},
    span::{LineIndex, Span},
};

/// Срез файла, который правило объявляет своим входом.
pub trait View<'a>: Sized {
    /// Строит срез по файлу, возвращая `None`, если срез неприменим.
    fn of(source: &'a Source) -> Option<Self>;
}

impl<'a, A: View<'a>, B: View<'a>> View<'a> for (A, B) {
    fn of(source: &'a Source) -> Option<(A, B)> {
        Some((A::of(source)?, B::of(source)?))
    }
}

impl<'a, A: View<'a>, B: View<'a>, C: View<'a>> View<'a> for (A, B, C) {
    fn of(source: &'a Source) -> Option<(A, B, C)> {
        Some((A::of(source)?, B::of(source)?, C::of(source)?))
    }
}

/// Дерево разбора с индексом узлов по виду.
#[derive(Clone, Copy)]
pub struct SyntaxView<'a> {
    /// Настройки прогона.
    pub settings: &'a Settings,
    index: &'a BTreeMap<SyntaxKind, Vec<SyntaxNode>>,
}

impl<'a> SyntaxView<'a> {
    /// Возвращает узлы заданного вида в порядке обхода.
    pub fn nodes(&self, kind: SyntaxKind) -> impl Iterator<Item = SyntaxNode> + 'a {
        self.index
            .get(&kind)
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .cloned()
    }
}

impl<'a> View<'a> for SyntaxView<'a> {
    fn of(source: &'a Source) -> Option<SyntaxView<'a>> {
        let parsed = source.parsed();
        Some(SyntaxView {
            settings: &source.settings,
            index: &parsed.index,
        })
    }
}

/// Комментарии файла.
#[derive(Clone, Copy)]
pub struct CommentView<'a> {
    /// Комментарии в порядке возрастания смещения.
    pub comments: &'a [CommentTok],
}

impl<'a> View<'a> for CommentView<'a> {
    fn of(source: &'a Source) -> Option<CommentView<'a>> {
        Some(CommentView {
            comments: &source.parsed().comments,
        })
    }
}

/// Строковые литералы файла.
#[derive(Clone, Copy)]
pub struct LiteralView<'a> {
    /// Литералы в порядке возрастания смещения.
    pub literals: &'a [LiteralTok],
}

impl<'a> View<'a> for LiteralView<'a> {
    fn of(source: &'a Source) -> Option<LiteralView<'a>> {
        Some(LiteralView {
            literals: &source.parsed().literals,
        })
    }
}

/// Физические строки файла.
#[derive(Clone, Copy)]
pub struct LineView<'a> {
    /// Текст файла.
    pub text: &'a str,
    /// Индекс строк.
    pub lines: &'a LineIndex,
    /// Настройки прогона.
    pub settings: &'a Settings,
}

impl<'a> LineView<'a> {
    /// Возвращает диапазон строки, заданной номером от нуля.
    pub fn span(&self, line: usize) -> Span {
        self.lines.line_span(self.text, line)
    }

    /// Возвращает текст строки без завершающего перевода строки.
    pub fn text_of(&self, line: usize) -> &'a str {
        let span = self.span(line);
        self.text
            .get(span.start as usize..span.end as usize)
            .unwrap_or_default()
    }

    /// Возвращает число строк.
    pub fn count(&self) -> usize {
        self.lines.line_count()
    }
}

impl<'a> View<'a> for LineView<'a> {
    fn of(source: &'a Source) -> Option<LineView<'a>> {
        Some(LineView {
            text: &source.text,
            lines: source.lines(),
            settings: &source.settings,
        })
    }
}
