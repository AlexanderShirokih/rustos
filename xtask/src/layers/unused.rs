//! Сверка объявленных path-зависимостей с кодом ищет рёбра без единой ссылки в исходниках.
//! Такое ребро искажает граф, по которому идёт проверка слоёв.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use anyhow::{Context, Result};

use crate::{
    layers::metadata::{CargoMetadata, MetaDependency, relative_dir},
    sources::rust_sources,
};

pub(super) fn unused_dependencies(metas: &[CargoMetadata], root: &Path) -> Result<Vec<String>> {
    let lib_names = lib_target_names(metas);
    let mut findings = BTreeSet::new();
    for meta in metas {
        for pkg in &meta.packages {
            let manifest_dir = pkg
                .manifest_path
                .parent()
                .with_context(|| format!("crate {}: manifest path has no parent", pkg.name))?;
            let paths: Vec<&MetaDependency> = pkg
                .dependencies
                .iter()
                .filter(|dep| dep.path.is_some())
                .collect();
            if paths.is_empty() {
                continue;
            }
            let sources = rust_sources(manifest_dir)?;
            for dep in paths {
                // В коде зависимость зовут либо переименованием из манифеста, либо именем её
                // lib-цели: у пакета оно вправе отличаться от собственного.
                let referenced = dep.rename.as_deref().unwrap_or_else(|| {
                    lib_names
                        .get(dep.name.as_str())
                        .map_or(dep.name.as_str(), |name| name.as_str())
                });
                if !mentions_crate(&sources, referenced) {
                    findings.insert(format!(
                        "{}: dependency `{}` is declared but never used in {}",
                        pkg.name,
                        dep.name,
                        relative_dir(&pkg.name, manifest_dir, root)?
                    ));
                }
            }
        }
    }
    Ok(findings.into_iter().collect())
}

fn lib_target_names(metas: &[CargoMetadata]) -> BTreeMap<&str, &String> {
    let mut names = BTreeMap::new();
    for meta in metas {
        for pkg in &meta.packages {
            let lib = pkg
                .targets
                .iter()
                .find(|target| target.kind.iter().any(|kind| kind.contains("lib")));
            if let Some(lib) = lib {
                names.insert(pkg.name.as_str(), &lib.name);
            }
        }
    }
    names
}

fn mentions_crate(sources: &[String], dep: &str) -> bool {
    let ident = dep.replace('-', "_");
    sources.iter().any(|text| {
        text.match_indices(&ident).any(|(at, _)| {
            let before = text[..at].chars().next_back();
            let after = text[at + ident.len()..].chars().next();
            !before.is_some_and(is_ident_char) && !after.is_some_and(is_ident_char)
        })
    })
}

fn is_ident_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::layers::metadata::{MetaPackage, MetaTarget};

    #[test]
    fn crate_identifier_is_matched_as_a_whole_word() {
        let sources = vec!["use ipc_test::MockEnd;\nlet x = my_ipc + 1;\n".to_string()];

        let mentioned = ["ipc-test", "ipc", "test"].map(|name| mentions_crate(&sources, name));

        assert_eq!(mentioned, [true, false, false]);
    }

    #[test]
    fn lib_target_name_wins_over_the_package_name() {
        let meta = CargoMetadata {
            packages: vec![MetaPackage {
                name: "demo-harness".to_string(),
                manifest_path: PathBuf::from("lib/demo-harness/Cargo.toml"),
                dependencies: Vec::new(),
                targets: vec![MetaTarget {
                    name: "demo_tests".to_string(),
                    kind: vec!["lib".to_string()],
                    src_path: PathBuf::from("lib/demo-harness/src/lib.rs"),
                }],
            }],
        };

        let names = lib_target_names(std::slice::from_ref(&meta));

        assert_eq!(
            names.get("demo-harness").map(|name| name.as_str()),
            Some("demo_tests")
        );
    }
}
