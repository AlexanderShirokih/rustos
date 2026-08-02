use std::path::Path;

use crate::{rule::Rule, source::Source};

/// Прогоняет одно правило по тексту, минуя git, конфигурацию и снапшот.
pub fn check_one(rule: &Rule, path: &Path, text: &str) -> Vec<String> {
    let source = Source::detached(path, text);
    let meta = &source.meta;
    if !rule.applies.scope.matches(meta) {
        return Vec::new();
    }

    let mut findings = Vec::new();
    rule.run(&source, &mut findings);

    let lines = source.lines();
    findings
        .into_iter()
        .filter(|finding| crate::engine::passes(rule, &source, finding.at))
        .map(|finding| {
            let position = lines.position(&source.text, finding.at.start);
            format!("{}:{} {}", position.line, position.column, finding.message)
        })
        .collect()
}
