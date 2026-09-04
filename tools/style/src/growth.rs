//! Сверка снапшота принятых нарушений с его записью в базовой ревизии: снапшот, выросший по
//! какому-либо правилу, уже объявленному в базовой ревизии, означает, что в него попало
//! нарушение нового кода. Правило, которого база не объявляла (ни в своём `style.toml`, ни в
//! своём каталоге скомпилированных правил), входит в снапшот целиком - сверять его рост не с чем.

use std::{
    collections::BTreeSet,
    io::ErrorKind,
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{Context as _, Result};

use crate::{
    baseline::{self, Baseline},
    rules::catalog,
};

const CATALOG_PATH: &str = "tools/style/src/rules/catalog.rs";

/// Правило, у которого снапшот принял больше записей, чем в базовой ревизии.
pub struct Grown {
    /// Идентификатор правила.
    pub rule: String,
    /// Записей в проверяемом снапшоте.
    pub accepted: usize,
    /// Записей в базовой ревизии.
    pub was: usize,
}

/// Правило, не объявленное в базовой ревизии: его снапшот принят целиком, без сверки роста.
pub struct NewRule {
    /// Идентификатор правила.
    pub rule: String,
    /// Записей в проверяемом снапшоте.
    pub accepted: usize,
}

/// Итог сверки снапшота рабочего дерева с его записью в базовой ревизии.
pub struct Comparison {
    /// Правила, объявленные уже в базовой ревизии, у которых снапшот вырос.
    pub grown: Vec<Grown>,
    /// Правила, которых базовая ревизия не объявляла: снапшот принят целиком.
    pub new: Vec<NewRule>,
}

/// Сверяет снапшот рабочего дерева с его записью в ревизии.
/// `None` означает, что ревизия записи не несёт и сверять не с чем.
pub fn grown_against(root: &Path, revision: &str) -> Result<Option<Comparison>> {
    let Some(recorded) = baseline_at(root, revision)? else {
        return Ok(None);
    };
    let declared = declared_at(root, revision)?;
    Ok(Some(compare(
        &Baseline::parse(&recorded),
        &head(root)?,
        &declared,
    )))
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
    show_at(root, revision, baseline::FILE_NAME)
}

fn declared_at(root: &Path, revision: &str) -> Result<BTreeSet<String>> {
    let config = show_at(root, revision, "style.toml")?;
    let catalog = show_at(root, revision, CATALOG_PATH)?;
    Ok(declared_by(config.as_deref(), catalog.as_deref()))
}

fn declared_by(config: Option<&str>, catalog: Option<&str>) -> BTreeSet<String> {
    let mut declared = match catalog {
        Some(text) => compiled_ids(text),
        None => catalog::compiled()
            .into_iter()
            .map(|rule| rule.meta.id)
            .collect(),
    };
    declared.extend(
        crate::engine::ENGINE_RULES
            .iter()
            .map(|id| (*id).to_owned()),
    );
    if let Some(text) = config {
        declared.extend(declared_ids(text));
    }
    declared
}

fn compiled_ids(text: &str) -> BTreeSet<String> {
    text.match_indices("::rule()")
        .filter_map(|(at, _)| {
            let tail: String = text[..at]
                .chars()
                .rev()
                .take_while(|ch| ch.is_alphanumeric() || *ch == '_')
                .collect();
            let id: String = tail.chars().rev().collect();
            (!id.is_empty()).then_some(id)
        })
        .collect()
}

fn declared_ids(text: &str) -> BTreeSet<String> {
    text.lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("id")?.trim_start();
            let rest = rest.strip_prefix('=')?.trim_start();
            let rest = rest.strip_prefix('"')?;
            let end = rest.find('"')?;
            Some(rest[..end].to_owned())
        })
        .collect()
}

fn show_at(root: &Path, revision: &str, path: &str) -> Result<Option<String>> {
    let output = Command::new("git")
        .args(["show", &format!("{revision}:{path}")])
        .current_dir(root)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .context("Failed to run git show")?;
    if !output.status.success() {
        // Ревизия старше самой записи, либо файл в ней не существовал: сверять не с чем.
        return Ok(None);
    }
    String::from_utf8(output.stdout)
        .context("git show returned non-UTF-8 output")
        .map(Some)
}

fn compare(base: &Baseline, head: &Baseline, declared: &BTreeSet<String>) -> Comparison {
    let base_counts = base.counts_by_rule();
    let mut grown = Vec::new();
    let mut new = Vec::new();
    for (rule, accepted) in head.counts_by_rule() {
        if !declared.contains(&rule) {
            new.push(NewRule { rule, accepted });
            continue;
        }
        let was = base_counts.get(&rule).copied().unwrap_or(0);
        if accepted > was {
            grown.push(Grown {
                rule,
                accepted,
                was,
            });
        }
    }
    Comparison { grown, new }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(keys: &[&str]) -> Baseline {
        Baseline::parse(&keys.join("\n"))
    }

    fn declared(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    const BASE_CATALOG: &str = concat!(
        "use crate::{rule::Rule, rules::{doc_on_declaration, expect_in_kernel}};\n",
        "pub fn compiled() -> Vec<Rule> {\n",
        "    vec![doc_on_declaration::rule(), expect_in_kernel::rule()]\n",
        "}\n",
    );

    #[test]
    fn compiled_ids_reads_module_names_of_the_catalog_body() {
        let ids = compiled_ids(BASE_CATALOG);

        assert_eq!(
            ids,
            BTreeSet::from([
                "doc_on_declaration".to_owned(),
                "expect_in_kernel".to_owned()
            ])
        );
    }

    #[test]
    fn rule_added_to_the_catalog_after_the_base_enters_as_new() {
        let known = declared_by(None, Some(BASE_CATALOG));
        let head = snapshot(&["comment_width|a.rs|one|1111"]);

        let comparison = compare(&Baseline::empty(), &head, &known);

        assert!(comparison.grown.is_empty());
        assert_eq!(comparison.new.len(), 1);
        assert_eq!(comparison.new[0].rule, "comment_width");
    }

    #[test]
    fn rule_present_in_the_base_catalog_is_compared_by_count() {
        let known = declared_by(None, Some(BASE_CATALOG));
        let head = snapshot(&["expect_in_kernel|a.rs|one|1111"]);

        let comparison = compare(&Baseline::empty(), &head, &known);

        assert!(comparison.new.is_empty());
        assert_eq!(comparison.grown.len(), 1);
        assert_eq!(comparison.grown[0].rule, "expect_in_kernel");
        assert_eq!(comparison.grown[0].was, 0);
    }

    #[test]
    fn base_without_a_catalog_keeps_the_catalog_of_the_working_tree() {
        let known = declared_by(None, None);

        for rule in catalog::compiled() {
            assert!(known.contains(&rule.meta.id), "{}", rule.meta.id);
        }
    }

    #[test]
    fn engine_rules_count_as_declared_by_any_base() {
        let known = declared_by(None, Some(BASE_CATALOG));

        assert!(known.contains("unused_style_allow"));
    }

    #[test]
    fn rule_with_more_entries_is_reported() {
        let base = snapshot(&["expect_in_kernel|a.rs|one|1111"]);
        let head = snapshot(&[
            "expect_in_kernel|a.rs|one|1111",
            "expect_in_kernel|b.rs|two|2222",
        ]);

        let found = compare(&base, &head, &declared(&["expect_in_kernel"])).grown;

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
        let known = declared(&["expect_in_kernel", "tuple_in_public_signature"]);

        let comparison = compare(&snapshot(&keys), &snapshot(&keys), &known);
        assert!(comparison.grown.is_empty());
        assert!(comparison.new.is_empty());
    }

    #[test]
    fn rule_missing_from_the_declared_set_is_accepted_as_new() {
        let base = snapshot(&["expect_in_kernel|a.rs|one|1111"]);
        let head = snapshot(&[
            "expect_in_kernel|a.rs|one|1111",
            "tuple_in_public_signature|b.rs|two|2222",
        ]);

        let comparison = compare(&base, &head, &declared(&["expect_in_kernel"]));

        assert!(comparison.grown.is_empty());
        assert_eq!(comparison.new.len(), 1);
        assert_eq!(comparison.new[0].rule, "tuple_in_public_signature");
        assert_eq!(comparison.new[0].accepted, 1);
    }

    #[test]
    fn declared_rule_with_no_base_entries_that_grew_is_reported() {
        let head = snapshot(&["tuple_in_public_signature|b.rs|two|2222"]);

        let found = compare(
            &Baseline::empty(),
            &head,
            &declared(&["tuple_in_public_signature"]),
        )
        .grown;

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].rule, "tuple_in_public_signature");
        assert_eq!(found[0].was, 0);
        assert_eq!(found[0].accepted, 1);
    }

    #[test]
    fn shrunk_rule_reports_nothing() {
        let base = snapshot(&[
            "expect_in_kernel|a.rs|one|1111",
            "expect_in_kernel|b.rs|two|2222",
        ]);
        let head = snapshot(&["expect_in_kernel|a.rs|one|1111"]);

        assert!(
            compare(&base, &head, &declared(&["expect_in_kernel"]))
                .grown
                .is_empty()
        );
    }

    #[test]
    fn empty_base_snapshot_reports_every_declared_rule() {
        let head = snapshot(&[
            "expect_in_kernel|a.rs|one|1111",
            "tuple_in_public_signature|b.rs|two|2222",
        ]);
        let known = declared(&["expect_in_kernel", "tuple_in_public_signature"]);

        assert_eq!(compare(&Baseline::empty(), &head, &known).grown.len(), 2);
    }

    #[test]
    fn revision_without_record_has_nothing_to_compare() {
        let absent = grown_against(Path::new("."), "no-such-revision-for-style-tests");

        assert!(matches!(absent, Ok(None)));
    }

    #[test]
    fn declared_ids_reads_plain_id_lines_regardless_of_toml_shape() {
        let text = concat!(
            "# comment\n",
            "[[text]]\n",
            "id = \"fronted_negation\"\n",
            "title = \"id looking value\"\n",
            "\n",
            "[[text]]\n",
            "id=\"postposed_sam\"\n",
        );

        let ids = declared_ids(text);

        assert_eq!(
            ids,
            BTreeSet::from(["fronted_negation".to_owned(), "postposed_sam".to_owned()])
        );
    }
}
