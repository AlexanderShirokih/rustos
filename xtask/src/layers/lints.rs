//! Сверка копий таблиц корневого манифеста в отделённых workspace: наследовать
//! `[workspace.lints]` и `[workspace.package]` вне корневого дерева нечем, а версия внешней
//! зависимости в `[workspace.dependencies]` иначе расходится молча.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::layers::metadata::read_manifest;

pub(super) fn check_lint_policy(root: &Path, detached: &[PathBuf]) -> Result<()> {
    let expected = read_manifest(&root.join("Cargo.toml"))?;
    for workspace in detached {
        let manifest = workspace.join("Cargo.toml");
        check_workspace_tables(&expected, &read_manifest(&manifest)?, &manifest)?;
    }
    Ok(())
}

fn check_workspace_tables(
    root: &toml::Table,
    manifest: &toml::Table,
    display: &Path,
) -> Result<()> {
    if workspace_table(manifest, "lints") != workspace_table(root, "lints") {
        bail!(
            "check-layers: {} declares a [workspace.lints] table that differs from the root one",
            display.display()
        );
    }
    if let Some(package) = workspace_table(manifest, "package")
        && Some(package) != workspace_table(root, "package")
    {
        bail!(
            "check-layers: {} declares a [workspace.package] table that differs from the root one",
            display.display()
        );
    }

    let root_dependencies = workspace_table(root, "dependencies")
        .and_then(toml::Value::as_table)
        .context("check-layers: the root manifest declares no [workspace.dependencies]")?;
    let dependencies = workspace_table(manifest, "dependencies")
        .and_then(toml::Value::as_table)
        .into_iter()
        .flatten();
    for (name, value) in dependencies {
        let Some(version) = external_version(value) else {
            continue;
        };
        let expected = root_dependencies
            .get(name)
            .and_then(external_version)
            .with_context(|| {
                format!(
                    "check-layers: {} declares external dependency `{name}` that the root manifest does not",
                    display.display()
                )
            })?;
        if version != expected {
            bail!(
                "check-layers: {} declares `{name}` version `{version}`, the root manifest declares `{expected}`",
                display.display()
            );
        }
    }
    Ok(())
}

fn workspace_table<'a>(manifest: &'a toml::Table, key: &str) -> Option<&'a toml::Value> {
    manifest
        .get("workspace")
        .and_then(toml::Value::as_table)?
        .get(key)
}

fn external_version(value: &toml::Value) -> Option<&str> {
    match value {
        toml::Value::String(version) => Some(version),
        toml::Value::Table(table) if !table.contains_key("path") => {
            table.get("version").and_then(toml::Value::as_str)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = "
[workspace.package]
version = '0.1.0'
edition = '2024'

[workspace.dependencies]
spin = '0.12.0'
serde = { version = '1.0', features = ['derive'] }
ipc = { path = 'lib/ipc' }

[workspace.lints.rust]
unsafe_code = 'deny'
";

    fn check(manifest: &str) -> Result<()> {
        let root = ROOT.parse()?;
        let manifest = manifest.parse()?;
        check_workspace_tables(&root, &manifest, Path::new("user/Cargo.toml"))
    }

    #[test]
    fn matching_tables_pass() {
        let manifest = "
[workspace.package]
version = '0.1.0'
edition = '2024'

[workspace.dependencies]
spin = '0.12.0'
serde = { version = '1.0' }
ipc = { path = '../lib/ipc' }

[workspace.lints.rust]
unsafe_code = 'deny'
";
        check(manifest).expect("tables match");
    }

    #[test]
    fn a_workspace_without_lints_is_refused() {
        let error = check("[workspace]\nmembers = []\n").unwrap_err();
        assert!(error.to_string().contains("[workspace.lints]"), "{error}");
    }

    #[test]
    fn a_different_package_table_is_refused() {
        let manifest = "
[workspace.package]
version = '0.2.0'
edition = '2024'

[workspace.lints.rust]
unsafe_code = 'deny'
";
        let error = check(manifest).unwrap_err();
        assert!(error.to_string().contains("[workspace.package]"), "{error}");
    }

    #[test]
    fn a_dependency_version_that_drifted_names_both_values() {
        let manifest = "
[workspace.dependencies]
spin = '0.10.0'

[workspace.lints.rust]
unsafe_code = 'deny'
";
        let error = check(manifest).unwrap_err().to_string();
        assert!(error.contains("user/Cargo.toml"), "{error}");
        assert!(error.contains("`spin`"), "{error}");
        assert!(
            error.contains("`0.10.0`") && error.contains("`0.12.0`"),
            "{error}"
        );
    }

    #[test]
    fn an_external_dependency_unknown_to_the_root_is_refused() {
        let manifest = "
[workspace.dependencies]
regex = '1'

[workspace.lints.rust]
unsafe_code = 'deny'
";
        let error = check(manifest).unwrap_err();
        assert!(error.to_string().contains("`regex`"), "{error}");
    }
}
