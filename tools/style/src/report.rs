use std::path::PathBuf;

use crate::{
    rule::Severity,
    span::{Position, Span},
};

/// Формат вывода отчёта.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    /// Человекочитаемые строки.
    Text,
    /// Машиночитаемый JSON.
    Json,
}

/// Находка, дополненная идентичностью и положением.
#[derive(Clone, Debug)]
pub struct Report {
    /// Идентификатор правила.
    pub rule: String,
    /// Действующий уровень.
    pub severity: Severity,
    /// Путь от корня репозитория.
    pub path: PathBuf,
    /// Диапазон находки в файле.
    pub at: Span,
    /// Положение начала находки.
    pub position: Position,
    /// Объявление, накрывающее находку.
    pub item: String,
    /// Отпечаток текста находки.
    pub fingerprint: String,
    /// Номер повтора среди находок с тем же отпечатком в том же объявлении.
    pub occurrence: u32,
    /// Формулировка претензии.
    pub message: String,
}

impl Report {
    /// Ключ снапшота: правило, путь, объявление, отпечаток, номер повтора.
    pub fn key(&self) -> String {
        let repeat = if self.occurrence > 1 {
            format!("#{}", self.occurrence)
        } else {
            String::new()
        };
        format!(
            "{}|{}|{}|{}{repeat}",
            self.rule,
            self.path.to_string_lossy(),
            self.item,
            self.fingerprint
        )
    }

    /// Строка человекочитаемого отчёта.
    pub fn line(&self) -> String {
        format!(
            "{}:{}:{} {} [{}] {}",
            self.path.to_string_lossy(),
            self.position.line,
            self.position.column,
            label(self.severity),
            self.rule,
            self.message
        )
    }

    /// Объект машиночитаемого отчёта.
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "rule": self.rule,
            "severity": label(self.severity),
            "path": self.path.to_string_lossy(),
            "line": self.position.line,
            "column": self.position.column,
            "item": self.item,
            "fingerprint": self.fingerprint,
            "message": self.message,
            "key": self.key(),
        })
    }
}

pub(crate) fn label(severity: Severity) -> &'static str {
    match severity {
        Severity::Warn => "warn",
        Severity::Deny => "deny",
    }
}

pub(crate) fn sort(reports: &mut [Report]) {
    reports.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then(left.position.cmp(&right.position))
            .then(left.rule.cmp(&right.rule))
            .then(left.fingerprint.cmp(&right.fingerprint))
            .then(left.occurrence.cmp(&right.occurrence))
    });
}
