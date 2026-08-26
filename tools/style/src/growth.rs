//! Сверка снапшота принятых нарушений с его записью в базовой ревизии: снапшот, выросший по
//! какому-либо правилу, означает, что в него попало нарушение нового кода.

use std::{
    io::ErrorKind,
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{Context as _, Result};

use crate::baseline::{self, Baseline};

/// Правило, у которого снапшот принял больше записей, чем в базовой ревизии.
pub struct Grown {
    /// Идентификатор правила.
    pub rule: String,
    /// Записей в проверяемом снапшоте.
    pub accepted: usize,
    /// Записей в базовой ревизии.
    pub was: usize,
}

/// Сверяет снапшот рабочего дерева с его записью в ревизии.
/// `None` означает, что ревизия записи не несёт и сверять не с чем.
pub fn grown_against(root: &Path, revision: &str) -> Result<Option<Vec<Grown>>> {
    let Some(recorded) = baseline_at(root, revision)? else {
        return Ok(None);
    };
    Ok(Some(grown(&Baseline::parse(&recorded), &head(root)?)))
}

fn head(root: &Path) -> Result<Baseline> {
    let path = root.join(baseline::FILE_NAME);
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(Baseline::parse(&text)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(Baseline::empty()),
        Err(error) => Err(error).with_context(|| format!("Failed to read {}", path.display())),
    }
}

fn baseline_at(root: &Path, revision: &str) -> Result<Option<String>> {
    let output = Command::new("git")
        .args(["show", &format!("{revision}:{}", baseline::FILE_NAME)])
        .current_dir(root)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .context("Failed to run git show")?;
    if !output.status.success() {
        // Ревизия старше самой записи: сверять не с чем.
        return Ok(None);
    }
    String::from_utf8(output.stdout)
        .context("git show returned non-UTF-8 output")
        .map(Some)
}

fn grown(base: &Baseline, head: &Baseline) -> Vec<Grown> {
    let base = base.counts_by_rule();
    head.counts_by_rule()
        .into_iter()
        .filter_map(|(rule, accepted)| {
            let was = base.get(&rule).copied().unwrap_or(0);
            (accepted > was).then_some(Grown {
                rule,
                accepted,
                was,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(keys: &[&str]) -> Baseline {
        Baseline::parse(&keys.join("\n"))
    }

    #[test]
    fn rule_with_more_entries_is_reported() {
        let base = snapshot(&["expect_in_kernel|a.rs|one|1111"]);
        let head = snapshot(&[
            "expect_in_kernel|a.rs|one|1111",
            "expect_in_kernel|b.rs|two|2222",
        ]);

        let found = grown(&base, &head);

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].rule, "expect_in_kernel");
        assert_eq!(found[0].accepted, 2);
        assert_eq!(found[0].was, 1);
    }

    #[test]
    fn unchanged_counts_report_nothing() {
        let keys = [
            "expect_in_kernel|a.rs|one|1111",
            "tuple_in_public_signature|b.rs|two|2222",
        ];

        assert!(grown(&snapshot(&keys), &snapshot(&keys)).is_empty());
    }

    #[test]
    fn rule_absent_from_base_is_reported() {
        let base = snapshot(&["expect_in_kernel|a.rs|one|1111"]);
        let head = snapshot(&[
            "expect_in_kernel|a.rs|one|1111",
            "tuple_in_public_signature|b.rs|two|2222",
        ]);

        let found = grown(&base, &head);

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].rule, "tuple_in_public_signature");
        assert_eq!(found[0].was, 0);
    }

    #[test]
    fn shrunk_rule_reports_nothing() {
        let base = snapshot(&[
            "expect_in_kernel|a.rs|one|1111",
            "expect_in_kernel|b.rs|two|2222",
        ]);
        let head = snapshot(&["expect_in_kernel|a.rs|one|1111"]);

        assert!(grown(&base, &head).is_empty());
    }

    #[test]
    fn empty_base_snapshot_reports_every_rule() {
        let head = snapshot(&[
            "expect_in_kernel|a.rs|one|1111",
            "tuple_in_public_signature|b.rs|two|2222",
        ]);

        assert_eq!(grown(&Baseline::empty(), &head).len(), 2);
    }

    #[test]
    fn revision_without_record_has_nothing_to_compare() {
        let absent = grown_against(Path::new("."), "no-such-revision-for-style-tests");

        assert!(matches!(absent, Ok(None)));
    }
}
