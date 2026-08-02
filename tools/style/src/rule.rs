use regex::Regex;

use crate::{scope::Applies, source::Source, span::Span};

/// Уровень находки.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Severity {
    /// Печатается, код возврата не меняет.
    Warn,
    /// Печатается и делает прогон неуспешным.
    Deny,
}

/// Семейство правила, задающее природу входа и потолок уровня.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Category {
    /// Вопрос к манифесту пакета.
    Manifest,
    /// Вопрос к дереву разбора.
    Ast,
    /// Вопрос к прозе комментария.
    Prose,
    /// Вопрос к структуре крейта.
    Structure,
    /// Вопрос к набору фактов об изменении.
    Process,
}

impl Category {
    /// Возвращает потолок уровня, выше которого правило семейства не поднимается.
    pub fn ceiling(self) -> Severity {
        match self {
            Category::Prose => Severity::Warn,
            _ => Severity::Deny,
        }
    }
}

/// Паспорт правила.
#[derive(Clone)]
pub struct RuleMeta {
    /// Идентификатор, он же имя файла правила и ключ конфигурации.
    pub id: String,
    /// Однострочное описание для отчёта.
    pub title: String,
    /// Семейство.
    pub category: Category,
    /// Уровень по умолчанию.
    pub severity: Severity,
}

/// Место, к которому правило предъявляет претензию.
#[derive(Clone, Debug)]
pub struct Finding {
    /// Диапазон в файле.
    pub at: Span,
    /// Формулировка претензии.
    pub message: String,
}

impl Finding {
    /// Собирает находку из диапазона и формулировки.
    pub fn new(at: Span, message: String) -> Finding {
        Finding { at, message }
    }
}

/// Единица текста, которой оперирует декларативное правило.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TextTarget {
    /// Любой комментарий.
    Comment,
    /// Только doc-комментарий.
    Doc,
    /// Физическая строка файла.
    Line,
}

/// Преобразование текста перед сопоставлением.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Strip {
    /// Содержимое обратных кавычек заменяется пробелами.
    Backticks,
}

/// Правило, объявленное данными: узор с сужающими фильтрами.
pub struct TextRule {
    /// Что считается единицей текста.
    pub target: TextTarget,
    /// Узор находки.
    pub pattern: Regex,
    /// Узоры, каждый из которых обязан совпасть с единицей.
    pub require: Vec<Regex>,
    /// Узоры, любой из которых снимает единицу с рассмотрения.
    pub unless: Vec<Regex>,
    /// Преобразования, применяемые до сопоставления.
    pub strip: Vec<Strip>,
    /// Формулировка претензии.
    pub message: String,
}

/// Форма проверки.
pub enum Check {
    /// Скомпилированная проверка одного файла.
    Compiled(fn(&Source, &mut Vec<Finding>)),
    /// Проверка, собранная из декларации в конфигурации.
    Text(TextRule),
}

/// Правило целиком: паспорт, область действия и проверка.
pub struct Rule {
    /// Паспорт.
    pub meta: RuleMeta,
    /// Область действия.
    pub applies: Applies,
    /// Проверка.
    pub check: Check,
}

impl Rule {
    /// Прогоняет проверку по файлу, не применяя фильтров области и контекста.
    pub fn run(&self, source: &Source, out: &mut Vec<Finding>) {
        match &self.check {
            Check::Compiled(run) => run(source, out),
            Check::Text(text) => crate::rules::text_rule::run(text, source, out),
        }
    }
}
