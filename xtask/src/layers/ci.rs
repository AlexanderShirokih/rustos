//! Сверка описаний проверок с реестром. Матрица архитектур в `ci-pr-checks.yml` повторяет
//! реестр, а команда проверки не называет крейт ни исключением, ни manifest-путём.

use std::{fs, path::Path};

use anyhow::{Context, Result, bail};

use crate::layers::metadata::ArchEntry;

const WORKFLOW: &str = ".github/workflows/ci-pr-checks.yml";
const CHECKS: &str = "checks.toml";
const FORBIDDEN_TOKENS: [&str; 2] = ["--exclude", "--manifest-path"];

pub(super) fn check_descriptions(root: &Path, registry: &[ArchEntry]) -> Result<()> {
    for file in [WORKFLOW, CHECKS] {
        let path = root.join(file);
        let text = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        check_tokens(&text, file)?;
        if file == WORKFLOW {
            check_matrix(&text, file, registry)?;
        }
    }
    Ok(())
}

fn check_tokens(text: &str, file: &str) -> Result<()> {
    for (index, line) in text.lines().enumerate() {
        if let Some(token) = FORBIDDEN_TOKENS
            .iter()
            .find(|token| line.split_whitespace().any(|word| word == **token))
        {
            bail!(
                "check-layers: {file}:{}: `{token}` names a crate; the check goes by workspace and profile",
                index + 1
            );
        }
    }
    Ok(())
}

fn check_matrix(text: &str, file: &str, registry: &[ArchEntry]) -> Result<()> {
    let workflow: serde_yaml::Value =
        serde_yaml::from_str(text).with_context(|| format!("Failed to parse {file}"))?;
    let jobs = workflow
        .get("jobs")
        .and_then(serde_yaml::Value::as_mapping)
        .with_context(|| format!("{file} declares no jobs"))?;

    let mut expected: Vec<&str> = registry.iter().map(|entry| entry.name.as_str()).collect();
    expected.sort_unstable();
    let mut matrices = 0;
    for (name, job) in jobs {
        let Some(archs) = job
            .get("strategy")
            .and_then(|strategy| strategy.get("matrix"))
            .and_then(|matrix| matrix.get("arch"))
        else {
            continue;
        };
        let name = name.as_str().unwrap_or_default();
        let mut listed: Vec<&str> = archs
            .as_sequence()
            .with_context(|| format!("{file}: job {name}: matrix.arch is not a list"))?
            .iter()
            .map(|arch| {
                arch.as_str()
                    .with_context(|| format!("{file}: job {name}: matrix.arch holds a non-string"))
            })
            .collect::<Result<_>>()?;
        listed.sort_unstable();
        if listed != expected {
            bail!(
                "check-layers: {file}: job {name} lists architectures [{}], the registry declares [{}]",
                listed.join(", "),
                expected.join(", ")
            );
        }
        matrices += 1;
    }
    if matrices == 0 {
        bail!("check-layers: {file} declares no job with a matrix.arch");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> Vec<ArchEntry> {
        vec![ArchEntry {
            name: "aarch64".to_string(),
            bare: "aarch64-unknown-none".to_string(),
            system: "targets/aarch64-unknown-molecular.json".to_string(),
        }]
    }

    #[test]
    fn a_matrix_that_lags_behind_the_registry_is_refused() {
        let text = "
jobs:
  clippy-target:
    strategy:
      matrix:
        arch: [aarch64]
";
        let mut registry = registry();
        registry.push(ArchEntry {
            name: "riscv64".to_string(),
            bare: "riscv64gc-unknown-none-elf".to_string(),
            system: "targets/riscv64-unknown-molecular.json".to_string(),
        });
        let error = check_matrix(text, WORKFLOW, &registry)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("[aarch64]") && error.contains("[aarch64, riscv64]"),
            "{error}"
        );
    }

    #[test]
    fn an_exclusion_by_crate_name_is_refused() {
        let error = check_tokens("run = \"cargo test --workspace --exclude hal\"\n", CHECKS)
            .unwrap_err()
            .to_string();
        assert!(error.contains("checks.toml:1"), "{error}");
        assert!(check_tokens("# `workspace.exclude` is the registry\n", CHECKS).is_ok());
    }
}
