use std::collections::HashMap;

use anyhow::{Result, anyhow, bail, ensure};
use userland::USERLAND_PAGE_SIZE;

use super::{image::validate_image_path, source::ImageProgram, workspace::WorkspacePackage};

const USERLAND_STACK_ALIGN: u64 = USERLAND_PAGE_SIZE;

#[derive(Clone, Debug)]
pub(super) struct ResolvedProgram {
    pub(super) path: String,
    pub(super) package: String,
    pub(super) bin: String,
    pub(super) boot: bool,
    pub(super) stack_size: u64,
}

impl ResolvedProgram {
    fn qualified(&self) -> String {
        format!("{}::{}", self.package, self.bin)
    }
}

pub(super) fn resolve_programs(
    programs: &[ImageProgram],
    packages: &HashMap<String, WorkspacePackage>,
) -> Result<Vec<ResolvedProgram>> {
    let mut resolved = Vec::with_capacity(programs.len());
    for program in programs {
        let package = packages.get(&program.package).ok_or_else(|| {
            anyhow!(
                "package '{}' from image manifest not found in workspace",
                program.package
            )
        })?;
        resolved.push(resolve_program(program, package)?);
    }

    for (index, program) in resolved.iter().enumerate() {
        for earlier in resolved.iter().take(index) {
            // Ключ уникальности - пара пакета и bin-таргета: одно имя таргета
            // могут объявить разные пакеты.
            ensure!(
                (&program.package, &program.bin) != (&earlier.package, &earlier.bin),
                "program identity '{}' is listed more than once in image manifest",
                program.qualified()
            );
        }
    }

    match resolved.iter().filter(|program| program.boot).count() {
        1 => {}
        0 => bail!("userland image must contain exactly one boot program; found none"),
        count => bail!("userland image must contain exactly one boot program; found {count}"),
    }

    Ok(resolved)
}

fn resolve_program(program: &ImageProgram, package: &WorkspacePackage) -> Result<ResolvedProgram> {
    ensure!(
        !program.package.is_empty(),
        "image manifest [[programs]] item must set a non-empty 'package'"
    );
    let stack_size = program.stack_size.or(package.stack_size).ok_or_else(|| {
        anyhow!(
            "package '{}' is missing required field 'stack_size': \
             set it in [package.metadata.userland] or override it in the image manifest",
            program.package
        )
    })?;
    let resolved = ResolvedProgram {
        path: program.path.clone(),
        package: program.package.clone(),
        bin: resolve_bin(program, package)?,
        boot: program.boot.unwrap_or(false),
        stack_size,
    };
    validate_program(&resolved)?;
    Ok(resolved)
}

fn resolve_bin(program: &ImageProgram, package: &WorkspacePackage) -> Result<String> {
    let available = || {
        let mut names: Vec<&str> = package.binaries.iter().map(String::as_str).collect();
        names.sort_unstable();
        names.join(", ")
    };
    match &program.bin {
        Some(bin) => {
            ensure!(
                package.binaries.iter().any(|target| target == bin),
                "package '{}' declares no bin target '{bin}'; it declares: {}",
                program.package,
                available()
            );
            Ok(bin.clone())
        }
        None => match package.binaries.as_slice() {
            [single] => Ok(single.clone()),
            [] => bail!(
                "package '{}' from image manifest declares no bin targets",
                program.package
            ),
            targets => bail!(
                "package '{}' declares {} bin targets; \
                 the image manifest must name one with 'bin': {}",
                program.package,
                targets.len(),
                available()
            ),
        },
    }
}

pub(super) fn validate_program(program: &ResolvedProgram) -> Result<()> {
    validate_image_path(&program.path)?;
    ensure!(
        program.stack_size != 0 && program.stack_size.is_multiple_of(USERLAND_STACK_ALIGN),
        "userland entry '{}' stack_size must be a non-zero multiple of {} bytes; got {}",
        program.path,
        USERLAND_STACK_ALIGN,
        program.stack_size
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use filesystem_abi::NAME_MAX;

    use super::{super::build::userland_binary_path, *};

    const BOOT_PATH: &str = "/boot/rootkeeper";

    fn image_program(package: &str, path: &str, boot: bool) -> ImageProgram {
        ImageProgram {
            package: package.to_string(),
            path: path.to_string(),
            bin: None,
            boot: Some(boot),
            stack_size: None,
        }
    }

    fn programs_of(programs: &[(&str, &str, bool)]) -> Vec<ImageProgram> {
        programs
            .iter()
            .map(|(package, path, boot)| image_program(package, path, *boot))
            .collect()
    }

    fn workspace_package(binaries: &[&str], stack_size: u64) -> WorkspacePackage {
        WorkspacePackage {
            binaries: binaries.iter().map(|name| (*name).to_string()).collect(),
            stack_size: Some(stack_size),
        }
    }

    fn metadata_map(packages: &[(&str, u64)]) -> HashMap<String, WorkspacePackage> {
        packages
            .iter()
            .map(|(package, stack_size)| {
                (
                    (*package).to_string(),
                    workspace_package(&[package], *stack_size),
                )
            })
            .collect()
    }

    #[test]
    fn resolve_takes_defaults_from_package_metadata() {
        let programs = programs_of(&[
            ("rootkeeper", BOOT_PATH, true),
            ("logger", "/boot/logger", false),
        ]);
        let metadata = metadata_map(&[("rootkeeper", 0x10000), ("logger", 0x4000)]);

        let programs = resolve_programs(&programs, &metadata).expect("resolve must succeed");
        assert_eq!(programs.len(), 2);
        assert_eq!(programs[0].path, BOOT_PATH);
        assert_eq!(programs[0].bin, "rootkeeper");
        assert!(programs[0].boot);
        assert_eq!(programs[0].stack_size, 0x10000);
        assert!(!programs[1].boot);
        assert_eq!(programs[1].stack_size, 0x4000);
    }

    #[test]
    fn resolve_prefers_image_overrides_over_metadata() {
        let mut programs = programs_of(&[("rootkeeper", BOOT_PATH, true)]);
        programs[0].bin = Some("rootkeeper-min".to_string());
        programs[0].stack_size = Some(0x8000);
        let metadata = HashMap::from([(
            "rootkeeper".to_string(),
            workspace_package(&["rootkeeper", "rootkeeper-min"], 0x10000),
        )]);

        let programs = resolve_programs(&programs, &metadata).expect("resolve must succeed");
        assert_eq!(programs[0].bin, "rootkeeper-min");
        assert_eq!(programs[0].stack_size, 0x8000);
    }

    #[test]
    fn resolve_takes_the_bin_target_from_the_only_one_declared() {
        let programs = programs_of(&[("clock", "/boot/clock", true)]);
        let metadata = HashMap::from([(
            "clock".to_string(),
            workspace_package(&["driver-pl031"], 0x4000),
        )]);

        let programs = resolve_programs(&programs, &metadata).expect("resolve must succeed");
        assert_eq!(programs[0].bin, "driver-pl031");
        assert_eq!(programs[0].qualified(), "clock::driver-pl031");
    }

    #[test]
    fn resolve_rejects_a_package_with_several_bin_targets_without_bin() {
        let programs = programs_of(&[("testrunner", "/boot/testrunner", true)]);
        let metadata = HashMap::from([(
            "testrunner".to_string(),
            workspace_package(&["testrunner", "fixture"], 0x4000),
        )]);

        let err = resolve_programs(&programs, &metadata).expect_err("an ambiguous package fails");
        assert_eq!(
            err.to_string(),
            "package 'testrunner' declares 2 bin targets; \
             the image manifest must name one with 'bin': fixture, testrunner"
        );
    }

    #[test]
    fn resolve_rejects_a_bin_the_package_does_not_declare() {
        let mut programs = programs_of(&[("testrunner", "/boot/testrunner", true)]);
        programs[0].bin = Some("ghost".to_string());
        let metadata = HashMap::from([(
            "testrunner".to_string(),
            workspace_package(&["testrunner", "fixture"], 0x4000),
        )]);

        let err = resolve_programs(&programs, &metadata).expect_err("an unknown target fails");
        assert_eq!(
            err.to_string(),
            "package 'testrunner' declares no bin target 'ghost'; it declares: fixture, testrunner"
        );
    }

    #[test]
    fn resolve_rejects_a_package_without_bin_targets() {
        let programs = programs_of(&[("runtime", "/boot/runtime", true)]);
        let metadata = HashMap::from([("runtime".to_string(), workspace_package(&[], 0x4000))]);

        let err = resolve_programs(&programs, &metadata).expect_err("a library package fails");
        assert_eq!(
            err.to_string(),
            "package 'runtime' from image manifest declares no bin targets"
        );
    }

    #[test]
    fn resolve_takes_a_path_that_renames_the_program() {
        let programs = programs_of(&[("rootkeeper", "/system/keeper", true)]);
        let metadata = metadata_map(&[("rootkeeper", 0x4000)]);

        let programs = resolve_programs(&programs, &metadata).expect("resolve must succeed");
        // `path` объявляет размещение целиком.
        assert_eq!(programs[0].path, "/system/keeper");
        assert_eq!(programs[0].bin, "rootkeeper");
    }

    #[test]
    fn resolve_rejects_package_missing_from_workspace() {
        let programs = programs_of(&[("ghost", "/boot/ghost", true)]);
        let metadata = metadata_map(&[("rootkeeper", 0x10000)]);

        let err = resolve_programs(&programs, &metadata).expect_err("unknown package must fail");
        assert_eq!(
            err.to_string(),
            "package 'ghost' from image manifest not found in workspace"
        );
    }

    #[test]
    fn resolve_rejects_missing_stack_size() {
        let programs = programs_of(&[("rootkeeper", BOOT_PATH, true)]);
        let mut metadata = metadata_map(&[("rootkeeper", 0x10000)]);
        metadata.get_mut("rootkeeper").unwrap().stack_size = None;

        let err = resolve_programs(&programs, &metadata).expect_err("missing stack_size must fail");
        assert!(
            err.to_string()
                .contains("package 'rootkeeper' is missing required field 'stack_size'")
        );
    }

    #[test]
    fn resolve_rejects_bad_stack_size() {
        let programs = programs_of(&[("rootkeeper", BOOT_PATH, true)]);
        let metadata = metadata_map(&[("rootkeeper", 123)]);

        let err = resolve_programs(&programs, &metadata).expect_err("bad stack_size must fail");
        assert!(
            err.to_string()
                .contains("stack_size must be a non-zero multiple of 4096 bytes; got 123")
        );
    }

    #[test]
    fn resolve_rejects_paths_outside_the_contract() {
        let metadata = metadata_map(&[("rootkeeper", 0x4000)]);
        let rejected = |path: &str| {
            let programs = programs_of(&[("rootkeeper", path, true)]);
            resolve_programs(&programs, &metadata)
                .expect_err("path outside the contract must fail")
                .to_string()
        };

        // Правила пути берутся из контракта: сборка отвергает путь, на котором
        // сервер не поднимет дерево имён.
        let overlong = String::from("/boot/") + &"n".repeat(NAME_MAX + 1);
        for path in [overlong.as_str(), "boot/rootkeeper", "/boot/../rootkeeper"] {
            assert!(rejected(path).starts_with(&format!("image path '{path}' must be")));
        }
    }

    #[test]
    fn resolve_rejects_zero_boot_programs() {
        let programs = programs_of(&[
            ("rootkeeper", BOOT_PATH, false),
            ("shell", "/boot/shell", false),
        ]);
        let metadata = metadata_map(&[("rootkeeper", 0x4000), ("shell", 0x4000)]);

        let err = resolve_programs(&programs, &metadata).expect_err("no boot must fail");
        assert_eq!(
            err.to_string(),
            "userland image must contain exactly one boot program; found none"
        );
    }

    #[test]
    fn resolve_rejects_multiple_boot_programs() {
        let programs = programs_of(&[
            ("rootkeeper", BOOT_PATH, true),
            ("shell", "/boot/shell", true),
        ]);
        let metadata = metadata_map(&[("rootkeeper", 0x4000), ("shell", 0x4000)]);

        let err = resolve_programs(&programs, &metadata).expect_err("two boot programs must fail");
        assert_eq!(
            err.to_string(),
            "userland image must contain exactly one boot program; found 2"
        );
    }

    #[test]
    fn resolve_rejects_a_repeated_qualified_identity() {
        let programs = programs_of(&[
            ("rootkeeper", BOOT_PATH, true),
            ("rootkeeper", "/boot/second", false),
        ]);
        let metadata = metadata_map(&[("rootkeeper", 0x4000)]);

        let err = resolve_programs(&programs, &metadata).expect_err("a repeated identity fails");
        assert_eq!(
            err.to_string(),
            "program identity 'rootkeeper::rootkeeper' is listed more than once in image manifest"
        );
    }

    #[test]
    fn resolve_allows_two_packages_with_one_bin_target_name() {
        let programs = programs_of(&[
            ("rootkeeper", BOOT_PATH, true),
            ("keeper-fork", "/boot/second", false),
        ]);
        let metadata = HashMap::from([
            (
                "rootkeeper".to_string(),
                workspace_package(&["rootkeeper"], 0x4000),
            ),
            (
                "keeper-fork".to_string(),
                workspace_package(&["rootkeeper"], 0x4000),
            ),
        ]);

        let resolved = resolve_programs(&programs, &metadata).expect("distinct packages allowed");
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].qualified(), "rootkeeper::rootkeeper");
        assert_eq!(resolved[1].qualified(), "keeper-fork::rootkeeper");
        assert_eq!(
            userland_binary_path(Path::new("/target"), "device", &resolved[1].bin),
            Path::new("/target/device/release/rootkeeper")
        );
    }

    #[test]
    fn resolve_allows_same_package_distinct_bins() {
        let mut programs = programs_of(&[
            ("testrunner", "/boot/testrunner", true),
            ("testrunner", "/boot/fixture", false),
        ]);
        programs[0].bin = Some("testrunner".to_string());
        programs[1].bin = Some("fixture".to_string());
        let metadata = HashMap::from([(
            "testrunner".to_string(),
            workspace_package(&["testrunner", "fixture"], 0x4000),
        )]);

        let resolved = resolve_programs(&programs, &metadata).expect("distinct bins allowed");
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[1].bin, "fixture");
        assert!(!resolved[1].boot);
    }
}
