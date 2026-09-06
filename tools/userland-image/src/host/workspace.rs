use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

use super::USERLAND_WORKSPACE;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct UserlandMetadata {
    stack_size: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct CargoMetadataOutput {
    packages: Vec<CargoPackage>,
    target_directory: PathBuf,
}

#[derive(Debug, Deserialize)]
struct CargoPackage {
    name: String,
    targets: Vec<CargoTarget>,
    #[serde(default)]
    metadata: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct CargoTarget {
    name: String,
    kind: Vec<String>,
}

#[derive(Debug)]
pub(super) struct WorkspacePackage {
    pub(super) binaries: Vec<String>,
    pub(super) stack_size: Option<u64>,
}

pub(super) struct WorkspaceMetadata {
    pub(super) packages: HashMap<String, WorkspacePackage>,
    pub(super) target_directory: PathBuf,
}

pub(super) fn userland_workspace(project_root: &Path) -> PathBuf {
    project_root.join(USERLAND_WORKSPACE)
}

pub(super) fn load_workspace_metadata(project_root: &Path) -> Result<WorkspaceMetadata> {
    let output = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(project_root)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .context("Failed to execute cargo metadata")?;
    ensure!(
        output.status.success(),
        "cargo metadata failed with {}",
        output.status
    );
    let parsed = parse_cargo_metadata(&output.stdout)?;
    let packages = collect_packages(parsed.packages)?;
    Ok(WorkspaceMetadata {
        packages,
        target_directory: parsed.target_directory,
    })
}

fn parse_cargo_metadata(json: &[u8]) -> Result<CargoMetadataOutput> {
    serde_json::from_slice(json).context("Failed to parse cargo metadata JSON")
}

fn collect_packages(packages: Vec<CargoPackage>) -> Result<HashMap<String, WorkspacePackage>> {
    packages
        .into_iter()
        .map(|package| {
            let metadata = userland_metadata(&package)?;
            let binaries: Vec<String> = package
                .targets
                .into_iter()
                .filter(|target| target.kind.iter().any(|kind| kind == "bin"))
                .map(|target| target.name)
                .collect();
            Ok((
                package.name,
                WorkspacePackage {
                    binaries,
                    stack_size: metadata.stack_size,
                },
            ))
        })
        .collect()
}

fn userland_metadata(package: &CargoPackage) -> Result<UserlandMetadata> {
    let Some(section) = package
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("userland"))
    else {
        return Ok(UserlandMetadata::default());
    };
    serde_json::from_value(section.clone()).with_context(|| {
        format!(
            "Invalid [package.metadata.userland] in package '{}'",
            package.name
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_metadata_reads_userland_section() {
        let json = br#"{"target_directory": "/workspace/target", "packages": [
            {"name": "rootkeeper",
             "targets": [{"name": "rootkeeper", "kind": ["bin"]}],
             "metadata": {"userland": {"stack_size": 65536}}},
            {"name": "memory",
             "targets": [{"name": "memory", "kind": ["lib"]}],
             "metadata": null}
        ]}"#;

        let parsed = parse_cargo_metadata(json).expect("cargo metadata must parse");
        assert_eq!(parsed.target_directory, PathBuf::from("/workspace/target"));
        let packages = collect_packages(parsed.packages).expect("userland metadata must parse");
        assert_eq!(packages["rootkeeper"].stack_size, Some(65536));
        assert_eq!(packages["memory"].stack_size, None);
    }

    #[test]
    fn workspace_metadata_collects_bin_targets_by_package() {
        let json = br#"{"target_directory": "/workspace/target", "packages": [
            {"name": "testrunner",
             "targets": [{"name": "testrunner", "kind": ["bin"]},
                         {"name": "spawn-fixture", "kind": ["bin"]},
                         {"name": "testrunner", "kind": ["lib"]},
                         {"name": "build-script-build", "kind": ["custom-build"]},
                         {"name": "demo", "kind": ["example"]}],
             "metadata": null},
            {"name": "runtime",
             "targets": [{"name": "runtime", "kind": ["lib"]}],
             "metadata": null}
        ]}"#;

        let parsed = parse_cargo_metadata(json).expect("cargo metadata must parse");
        let packages = collect_packages(parsed.packages).expect("targets must collect");
        assert_eq!(
            packages["testrunner"].binaries,
            ["testrunner", "spawn-fixture"]
        );
        assert!(packages["runtime"].binaries.is_empty());
    }

    #[test]
    fn workspace_metadata_rejects_a_userland_bin_key() {
        let json = br#"{"target_directory": "/workspace/target", "packages": [
            {"name": "rootkeeper",
             "targets": [{"name": "rootkeeper", "kind": ["bin"]}],
             "metadata": {"userland": {"bin": "rootkeeper", "stack_size": 65536}}}
        ]}"#;

        let parsed = parse_cargo_metadata(json).expect("cargo metadata must parse");
        // Имя таргета приходит из вывода cargo metadata.
        let err = collect_packages(parsed.packages).expect_err("the removed key must fail");
        assert!(
            err.to_string()
                .contains("Invalid [package.metadata.userland] in package 'rootkeeper'")
        );
    }

    #[test]
    fn workspace_metadata_rejects_unknown_userland_field() {
        let json = br#"{"target_directory": "/workspace/target", "packages": [
            {"name": "rootkeeper",
             "targets": [{"name": "rootkeeper", "kind": ["bin"]}],
             "metadata": {"userland": {"bootstrap": true, "stack_size": 65536}}}
        ]}"#;

        let parsed = parse_cargo_metadata(json).expect("cargo metadata must parse");
        let err = collect_packages(parsed.packages).expect_err("unknown field must fail");
        assert!(
            err.to_string()
                .contains("Invalid [package.metadata.userland] in package 'rootkeeper'")
        );
    }
}
