use std::{
    collections::HashMap,
    ffi::OsStr,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use object::{Architecture, BinaryFormat, Object, ObjectKind, ObjectSegment, SegmentFlags, elf};
use serde::Deserialize;
#[cfg(test)]
use userland::EntryView;
use userland::{
    Entry, Segment, SegmentPermissions, USERLAND_ENTRY_NAME_CAPACITY, USERLAND_PAGE_SIZE,
};
use userland_image::{USERLAND_IMAGE_VERSION, decode, encode};

const USERLAND_MANIFEST_VERSION: u16 = USERLAND_IMAGE_VERSION;
const USERLAND_STACK_ALIGN: u64 = USERLAND_PAGE_SIZE;

/// Композиция образа: версия и список включаемых пакетов workspace.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImageManifest {
    #[serde(default = "default_manifest_version")]
    version: u16,
    programs: Vec<ImageProgram>,
}

/// Элемент композиции: имя пакета плюс точечные override полей метаданных.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImageProgram {
    package: String,
    name: Option<String>,
    bin: Option<String>,
    bootstrap: Option<bool>,
    stack_size: Option<u64>,
}

/// Свойства программы из секции `[package.metadata.userland]` её Cargo.toml.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct UserlandMetadata {
    name: Option<String>,
    bin: Option<String>,
    bootstrap: Option<bool>,
    stack_size: Option<u64>,
}

/// Программа после слияния метаданных пакета и override образа.
#[derive(Clone, Debug)]
struct ResolvedProgram {
    name: String,
    package: String,
    bin: String,
    bootstrap: bool,
    stack_size: u64,
}

#[derive(Debug, Deserialize)]
struct CargoMetadataOutput {
    packages: Vec<CargoPackage>,
}

#[derive(Debug, Deserialize)]
struct CargoPackage {
    name: String,
    #[serde(default)]
    metadata: Option<serde_json::Value>,
}

#[derive(Clone, Debug)]
struct BuiltEntry {
    image_order: usize,
    program: ResolvedProgram,
    entry_va: u64,
    segments: Vec<BuiltSegment>,
}

#[derive(Clone, Debug)]
struct BuiltSegment {
    bytes: Vec<u8>,
    va_base: u64,
    mem_size: u64,
    permissions: SegmentPermissions,
}

fn default_manifest_version() -> u16 {
    USERLAND_MANIFEST_VERSION
}

pub fn build_userland(
    project_root: &Path,
    manifest_path: &Path,
    build_dir: &Path,
) -> Result<PathBuf> {
    let manifest = load_manifest(manifest_path)?;
    let metadata = load_workspace_metadata(project_root)?;
    let programs = resolve_programs(&manifest, &metadata)?;
    let built_entries = programs
        .iter()
        .enumerate()
        .map(|(image_order, program)| build_program(project_root, image_order, program))
        .collect::<Result<Vec<_>>>()?;

    let blob = assemble_userland_image(&built_entries)?;
    let output_path = build_dir.join("userland.img");
    fs::write(&output_path, blob)
        .with_context(|| format!("Failed to write userland image '{}'", output_path.display()))?;
    Ok(output_path)
}

fn load_manifest(path: &Path) -> Result<ImageManifest> {
    let manifest_src = fs::read_to_string(path).with_context(|| {
        format!(
            "Failed to read userland image manifest '{}'",
            path.display()
        )
    })?;
    parse_manifest(&manifest_src)
}

fn parse_manifest(src: &str) -> Result<ImageManifest> {
    let manifest: ImageManifest =
        toml::from_str(src).context("Failed to parse userland image manifest TOML")?;
    ensure!(
        manifest.version == USERLAND_MANIFEST_VERSION,
        "Unsupported userland image manifest version {}; expected {}",
        manifest.version,
        USERLAND_MANIFEST_VERSION
    );
    ensure!(
        !manifest.programs.is_empty(),
        "userland image manifest must contain at least one [[programs]] item"
    );
    Ok(manifest)
}

fn load_workspace_metadata(project_root: &Path) -> Result<HashMap<String, UserlandMetadata>> {
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
    parse_workspace_metadata(&output.stdout)
}

fn parse_workspace_metadata(json: &[u8]) -> Result<HashMap<String, UserlandMetadata>> {
    let parsed: CargoMetadataOutput =
        serde_json::from_slice(json).context("Failed to parse cargo metadata JSON")?;
    parsed
        .packages
        .into_iter()
        .map(|package| {
            let metadata = userland_metadata(&package)?;
            Ok((package.name, metadata))
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

fn resolve_programs(
    manifest: &ImageManifest,
    metadata: &HashMap<String, UserlandMetadata>,
) -> Result<Vec<ResolvedProgram>> {
    let mut resolved = Vec::with_capacity(manifest.programs.len());
    for program in &manifest.programs {
        let meta = metadata.get(&program.package).ok_or_else(|| {
            anyhow!(
                "package '{}' from image manifest not found in workspace",
                program.package
            )
        })?;
        resolved.push(resolve_program(program, meta)?);
    }

    for (index, program) in resolved.iter().enumerate() {
        for earlier in &resolved[..index] {
            ensure!(
                program.package != earlier.package || program.bin != earlier.bin,
                "binary '{}::{}' is listed more than once in image manifest",
                program.package,
                program.bin
            );
            ensure!(
                program.name != earlier.name,
                "userland entry name '{}' is duplicated in image manifest",
                program.name
            );
        }
    }

    match resolved.iter().filter(|program| program.bootstrap).count() {
        1 => {}
        0 => bail!("userland image must contain exactly one bootstrap program; found none"),
        count => bail!("userland image must contain exactly one bootstrap program; found {count}"),
    }

    Ok(resolved)
}

fn resolve_program(program: &ImageProgram, meta: &UserlandMetadata) -> Result<ResolvedProgram> {
    ensure!(
        !program.package.is_empty(),
        "image manifest [[programs]] item must set a non-empty 'package'"
    );
    let stack_size = program.stack_size.or(meta.stack_size).ok_or_else(|| {
        anyhow!(
            "package '{}' is missing required field 'stack_size': \
             set it in [package.metadata.userland] or override it in the image manifest",
            program.package
        )
    })?;
    let resolved = ResolvedProgram {
        name: program
            .name
            .clone()
            .or_else(|| meta.name.clone())
            .unwrap_or_else(|| program.package.clone()),
        package: program.package.clone(),
        bin: program
            .bin
            .clone()
            .or_else(|| meta.bin.clone())
            .unwrap_or_else(|| program.package.clone()),
        bootstrap: program.bootstrap.or(meta.bootstrap).unwrap_or(false),
        stack_size,
    };
    validate_program(&resolved)?;
    Ok(resolved)
}

fn validate_program(program: &ResolvedProgram) -> Result<()> {
    ensure!(
        !program.name.is_empty() && program.name.len() <= USERLAND_ENTRY_NAME_CAPACITY,
        "userland entry '{}' name must be 1..={} bytes",
        program.name,
        USERLAND_ENTRY_NAME_CAPACITY
    );
    ensure!(
        program.name.is_ascii(),
        "userland entry '{}' name must be ASCII",
        program.name
    );
    ensure!(
        !program.bin.is_empty(),
        "userland entry '{}' bin must not be empty",
        program.name
    );
    ensure!(
        program.stack_size != 0 && program.stack_size.is_multiple_of(USERLAND_STACK_ALIGN),
        "userland entry '{}' stack_size must be a non-zero multiple of {} bytes; got {}",
        program.name,
        USERLAND_STACK_ALIGN,
        program.stack_size
    );
    Ok(())
}

fn build_program(
    project_root: &Path,
    image_order: usize,
    program: &ResolvedProgram,
) -> Result<BuiltEntry> {
    run_userland_build(project_root, program)?;
    let binary_path = userland_binary_path(project_root, &program.bin);
    read_and_extract_entry(image_order, &binary_path, program)
}

fn run_userland_build(project_root: &Path, program: &ResolvedProgram) -> Result<()> {
    let status = Command::new("cargo")
        .args([
            "build",
            "--package",
            &program.package,
            "--bin",
            &program.bin,
            "--target",
            "aarch64-unknown-none",
            "--release",
        ])
        .current_dir(project_root)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("Failed to execute cargo build for userland entry")?;

    if !status.success() {
        bail!(
            "Failed to build userland entry '{}' with status {}",
            program.name,
            status
        );
    }

    Ok(())
}

fn userland_binary_path(project_root: &Path, bin: &str) -> PathBuf {
    project_root
        .join("target/aarch64-unknown-none/release")
        .join(bin)
}

fn read_and_extract_entry(
    image_order: usize,
    path: &Path,
    program: &ResolvedProgram,
) -> Result<BuiltEntry> {
    let mut payload = Vec::new();
    File::open(path)
        .with_context(|| format!("Failed to open userland ELF '{}'", path.display()))?
        .read_to_end(&mut payload)
        .with_context(|| format!("Failed to read userland ELF '{}'", path.display()))?;

    ensure!(
        path.extension() != Some(OsStr::new("d")),
        "unexpected dep-info path for userland entry '{}'",
        program.name
    );

    let file = object::File::parse(payload.as_slice()).with_context(|| {
        format!(
            "Failed to parse payload ELF for userland entry '{}'",
            program.name
        )
    })?;

    validate_file(&file, program)?;
    let entry_va = file.entry();
    let segments = extract_segments(&file, program)?;

    Ok(BuiltEntry {
        image_order,
        program: program.clone(),
        entry_va,
        segments,
    })
}

fn validate_file(file: &object::File<'_>, program: &ResolvedProgram) -> Result<()> {
    ensure!(
        file.format() == BinaryFormat::Elf,
        "userland entry '{}' payload is not ELF",
        program.name
    );
    ensure!(
        file.architecture() == Architecture::Aarch64,
        "userland entry '{}' payload must target AArch64",
        program.name
    );
    ensure!(
        file.kind() == ObjectKind::Executable,
        "userland entry '{}' payload must be ET_EXEC",
        program.name
    );
    ensure!(
        file.is_little_endian(),
        "userland entry '{}' payload must be little-endian",
        program.name
    );
    ensure!(
        file.is_64(),
        "userland entry '{}' payload must be ELF64",
        program.name
    );
    Ok(())
}

fn extract_segments(
    file: &object::File<'_>,
    program: &ResolvedProgram,
) -> Result<Vec<BuiltSegment>> {
    let mut segments = Vec::new();

    for segment in file.segments() {
        let bytes = segment
            .data()
            .with_context(|| format!("Failed to read PT_LOAD bytes for '{}'", program.name))?;
        let permissions = map_segment_permissions(segment.flags(), program)?;
        segments.push(BuiltSegment {
            bytes: bytes.to_vec(),
            va_base: segment.address(),
            mem_size: segment.size(),
            permissions,
        });
    }

    ensure!(
        !segments.is_empty(),
        "userland entry '{}' must contain at least one PT_LOAD segment",
        program.name
    );

    Ok(segments)
}

fn map_segment_permissions(
    flags: SegmentFlags,
    program: &ResolvedProgram,
) -> Result<SegmentPermissions> {
    let SegmentFlags::Elf { p_flags } = flags else {
        bail!(
            "userland entry '{}' must expose ELF PT_LOAD segment flags",
            program.name
        );
    };

    let readable = (p_flags & elf::PF_R) != 0;
    let writable = (p_flags & elf::PF_W) != 0;
    let executable = (p_flags & elf::PF_X) != 0;

    match (readable, writable, executable) {
        (true, true, false) => Ok(SegmentPermissions::ReadWrite),
        (true, false, false) => Ok(SegmentPermissions::ReadOnly),
        (true, false, true) => Ok(SegmentPermissions::ReadExecute),
        _ => bail!(
            "userland entry '{}' has unsupported PT_LOAD flags {p_flags:#x}",
            program.name
        ),
    }
}

fn assemble_userland_image(entries: &[BuiltEntry]) -> Result<Vec<u8>> {
    validate_built_entries(entries)?;

    let ordered = order_entries(entries);

    let segments: Vec<Vec<Segment<'_>>> = ordered
        .iter()
        .map(|entry| {
            entry
                .segments
                .iter()
                .map(|segment| Segment {
                    va_base: segment.va_base,
                    mem_size: segment.mem_size,
                    permissions: segment.permissions,
                    bytes: &segment.bytes,
                })
                .collect()
        })
        .collect();
    let inputs: Vec<Entry<'_>> = ordered
        .iter()
        .zip(&segments)
        .map(|(entry, segments)| Entry {
            name: &entry.program.name,
            entry_va: entry.entry_va,
            stack_size: entry.program.stack_size,
            segments,
        })
        .collect();

    let image =
        encode(&inputs).map_err(|err| anyhow!("failed to serialize userland image: {err:?}"))?;

    decode(&image).map_err(|err| anyhow!("assembled userland image is invalid: {err:?}"))?;
    Ok(image)
}

fn validate_built_entries(entries: &[BuiltEntry]) -> Result<()> {
    ensure!(
        !entries.is_empty(),
        "userland image must contain at least one entry"
    );

    for entry in entries {
        validate_program(&entry.program)?;
        ensure!(
            !entry.segments.is_empty(),
            "userland entry '{}' must contain at least one segment",
            entry.program.name
        );
    }

    let bootstrap_entries = entries
        .iter()
        .filter(|entry| entry.program.bootstrap)
        .collect::<Vec<_>>();
    match bootstrap_entries.len() {
        1 => {
            let bootstrap = bootstrap_entries[0];
            ensure!(
                bootstrap
                    .segments
                    .iter()
                    .any(|segment| !segment.bytes.is_empty()),
                "bootstrap entry '{}' payload must not be empty",
                bootstrap.program.name
            );
        }
        0 => bail!("userland image must contain exactly one bootstrap entry; found none"),
        count => bail!("userland image must contain exactly one bootstrap entry; found {count}"),
    }

    Ok(())
}

fn order_entries(entries: &[BuiltEntry]) -> Vec<&BuiltEntry> {
    let mut ordered = entries.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|entry| (!entry.program.bootstrap, entry.image_order));
    ordered
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image_program(package: &str) -> ImageProgram {
        ImageProgram {
            package: package.to_string(),
            name: None,
            bin: None,
            bootstrap: None,
            stack_size: None,
        }
    }

    fn package_metadata(bootstrap: bool, stack_size: u64) -> UserlandMetadata {
        UserlandMetadata {
            name: None,
            bin: None,
            bootstrap: Some(bootstrap),
            stack_size: Some(stack_size),
        }
    }

    fn metadata_map(packages: &[(&str, bool, u64)]) -> HashMap<String, UserlandMetadata> {
        packages
            .iter()
            .map(|(package, bootstrap, stack_size)| {
                (
                    (*package).to_string(),
                    package_metadata(*bootstrap, *stack_size),
                )
            })
            .collect()
    }

    fn manifest_of(packages: &[&str]) -> ImageManifest {
        ImageManifest {
            version: USERLAND_MANIFEST_VERSION,
            programs: packages
                .iter()
                .map(|package| image_program(package))
                .collect(),
        }
    }

    fn resolved_program(name: &str, bootstrap: bool, stack_size: u64) -> ResolvedProgram {
        ResolvedProgram {
            name: name.to_string(),
            package: name.to_string(),
            bin: name.to_string(),
            bootstrap,
            stack_size,
        }
    }

    fn built_segment(
        bytes: &[u8],
        va_base: u64,
        mem_size: u64,
        permissions: SegmentPermissions,
    ) -> BuiltSegment {
        BuiltSegment {
            bytes: bytes.to_vec(),
            va_base,
            mem_size,
            permissions,
        }
    }

    fn built_entry(
        image_order: usize,
        name: &str,
        bootstrap: bool,
        entry_va: u64,
        segments: &[BuiltSegment],
    ) -> BuiltEntry {
        BuiltEntry {
            image_order,
            program: resolved_program(name, bootstrap, 0x4000),
            entry_va,
            segments: segments.to_vec(),
        }
    }

    #[test]
    fn manifest_parses_composition_with_overrides() {
        let manifest = parse_manifest(
            "version = 1\n\
             \n\
             [[programs]]\n\
             package = \"rootkeeper\"\n\
             \n\
             [[programs]]\n\
             package = \"testrunner\"\n\
             name = \"tests\"\n\
             stack_size = 131072\n",
        )
        .expect("composition manifest must parse");

        assert_eq!(manifest.version, 1);
        assert_eq!(manifest.programs.len(), 2);
        assert_eq!(manifest.programs[0].package, "rootkeeper");
        assert_eq!(manifest.programs[0].stack_size, None);
        assert_eq!(manifest.programs[1].name.as_deref(), Some("tests"));
        assert_eq!(manifest.programs[1].stack_size, Some(131_072));
    }

    #[test]
    fn manifest_rejects_legacy_entries_format() {
        let err = parse_manifest(
            "version = 1\n\
             \n\
             [[entries]]\n\
             name = \"rootkeeper\"\n\
             package = \"rootkeeper\"\n\
             bin = \"rootkeeper\"\n\
             bootstrap = true\n\
             stack_size = 65536\n",
        )
        .expect_err("legacy entries format must fail");
        assert!(
            err.to_string()
                .contains("Failed to parse userland image manifest TOML")
        );
    }

    #[test]
    fn manifest_rejects_unsupported_version() {
        let err = parse_manifest("version = 2\n\n[[programs]]\npackage = \"rootkeeper\"\n")
            .expect_err("unsupported version must fail");
        assert_eq!(
            err.to_string(),
            "Unsupported userland image manifest version 2; expected 1"
        );
    }

    #[test]
    fn manifest_rejects_empty_programs() {
        let err = parse_manifest("version = 1\nprograms = []\n")
            .expect_err("empty composition must fail");
        assert_eq!(
            err.to_string(),
            "userland image manifest must contain at least one [[programs]] item"
        );
    }

    #[test]
    fn resolve_takes_defaults_from_package_metadata() {
        let manifest = manifest_of(&["rootkeeper", "logger"]);
        let metadata = metadata_map(&[("rootkeeper", true, 0x10000), ("logger", false, 0x4000)]);

        let programs = resolve_programs(&manifest, &metadata).expect("resolve must succeed");
        assert_eq!(programs.len(), 2);
        assert_eq!(programs[0].name, "rootkeeper");
        assert_eq!(programs[0].bin, "rootkeeper");
        assert!(programs[0].bootstrap);
        assert_eq!(programs[0].stack_size, 0x10000);
        assert!(!programs[1].bootstrap);
        assert_eq!(programs[1].stack_size, 0x4000);
    }

    #[test]
    fn resolve_prefers_image_overrides_over_metadata() {
        let mut manifest = manifest_of(&["rootkeeper"]);
        manifest.programs[0].name = Some("keeper".to_string());
        manifest.programs[0].bin = Some("rootkeeper-min".to_string());
        manifest.programs[0].stack_size = Some(0x8000);
        let metadata = metadata_map(&[("rootkeeper", true, 0x10000)]);

        let programs = resolve_programs(&manifest, &metadata).expect("resolve must succeed");
        assert_eq!(programs[0].name, "keeper");
        assert_eq!(programs[0].bin, "rootkeeper-min");
        assert!(programs[0].bootstrap);
        assert_eq!(programs[0].stack_size, 0x8000);
    }

    #[test]
    fn resolve_rejects_package_missing_from_workspace() {
        let manifest = manifest_of(&["ghost"]);
        let metadata = metadata_map(&[("rootkeeper", true, 0x10000)]);

        let err = resolve_programs(&manifest, &metadata).expect_err("unknown package must fail");
        assert_eq!(
            err.to_string(),
            "package 'ghost' from image manifest not found in workspace"
        );
    }

    #[test]
    fn resolve_rejects_missing_stack_size() {
        let manifest = manifest_of(&["rootkeeper"]);
        let mut metadata = metadata_map(&[("rootkeeper", true, 0x10000)]);
        metadata.get_mut("rootkeeper").unwrap().stack_size = None;

        let err = resolve_programs(&manifest, &metadata).expect_err("missing stack_size must fail");
        assert!(
            err.to_string()
                .contains("package 'rootkeeper' is missing required field 'stack_size'")
        );
    }

    #[test]
    fn resolve_rejects_bad_stack_size() {
        let manifest = manifest_of(&["rootkeeper"]);
        let metadata = metadata_map(&[("rootkeeper", true, 123)]);

        let err = resolve_programs(&manifest, &metadata).expect_err("bad stack_size must fail");
        assert!(
            err.to_string()
                .contains("stack_size must be a non-zero multiple of 4096 bytes; got 123")
        );
    }

    #[test]
    fn resolve_rejects_zero_bootstrap_programs() {
        let manifest = manifest_of(&["rootkeeper", "shell"]);
        let metadata = metadata_map(&[("rootkeeper", false, 0x4000), ("shell", false, 0x4000)]);

        let err = resolve_programs(&manifest, &metadata).expect_err("no bootstrap must fail");
        assert_eq!(
            err.to_string(),
            "userland image must contain exactly one bootstrap program; found none"
        );
    }

    #[test]
    fn resolve_rejects_multiple_bootstrap_programs() {
        let manifest = manifest_of(&["rootkeeper", "shell"]);
        let metadata = metadata_map(&[("rootkeeper", true, 0x4000), ("shell", true, 0x4000)]);

        let err = resolve_programs(&manifest, &metadata).expect_err("two bootstraps must fail");
        assert_eq!(
            err.to_string(),
            "userland image must contain exactly one bootstrap program; found 2"
        );
    }

    #[test]
    fn resolve_rejects_duplicate_package() {
        let manifest = manifest_of(&["rootkeeper", "rootkeeper"]);
        let metadata = metadata_map(&[("rootkeeper", true, 0x4000)]);

        let err = resolve_programs(&manifest, &metadata).expect_err("duplicate package must fail");
        assert_eq!(
            err.to_string(),
            "binary 'rootkeeper::rootkeeper' is listed more than once in image manifest"
        );
    }

    #[test]
    fn resolve_allows_same_package_distinct_bins() {
        let manifest = ImageManifest {
            version: USERLAND_MANIFEST_VERSION,
            programs: vec![
                image_program("testrunner"),
                ImageProgram {
                    package: "testrunner".to_string(),
                    name: Some("fixture".to_string()),
                    bin: Some("fixture".to_string()),
                    bootstrap: Some(false),
                    stack_size: None,
                },
            ],
        };
        let metadata = metadata_map(&[("testrunner", true, 0x4000)]);

        let resolved = resolve_programs(&manifest, &metadata).expect("distinct bins allowed");
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[1].bin, "fixture");
        assert!(!resolved[1].bootstrap);
    }

    #[test]
    fn workspace_metadata_reads_userland_section() {
        let json = br#"{"packages": [
            {"name": "rootkeeper",
             "metadata": {"userland": {"bootstrap": true, "stack_size": 65536}}},
            {"name": "memory", "metadata": null}
        ]}"#;

        let metadata = parse_workspace_metadata(json).expect("metadata must parse");
        let rootkeeper = &metadata["rootkeeper"];
        assert_eq!(rootkeeper.bootstrap, Some(true));
        assert_eq!(rootkeeper.stack_size, Some(65536));

        let memory = &metadata["memory"];
        assert_eq!(memory.bootstrap, None);
        assert_eq!(memory.stack_size, None);
    }

    #[test]
    fn workspace_metadata_rejects_unknown_userland_field() {
        let json = br#"{"packages": [
            {"name": "rootkeeper",
             "metadata": {"userland": {"bootstrap": true, "stak_size": 65536}}}
        ]}"#;

        let err = parse_workspace_metadata(json).expect_err("typo in metadata must fail");
        assert!(
            err.to_string()
                .contains("Invalid [package.metadata.userland] in package 'rootkeeper'")
        );
    }

    #[test]
    fn bootstrap_entry_must_not_be_empty() {
        let entries = [built_entry(
            0,
            "rootkeeper",
            true,
            0x4000_0000,
            &[built_segment(
                &[],
                0x4000_0000,
                0x1000,
                SegmentPermissions::ReadExecute,
            )],
        )];
        let err = assemble_userland_image(&entries).expect_err("empty bootstrap must fail");
        assert_eq!(
            err.to_string(),
            "bootstrap entry 'rootkeeper' payload must not be empty"
        );
    }

    #[test]
    fn bootstrap_is_always_serialized_as_entries_zero() {
        let entries = [
            built_entry(
                2,
                "shell",
                false,
                0x5000_0000,
                &[built_segment(
                    b"ELF",
                    0x5000_0000,
                    0x1000,
                    SegmentPermissions::ReadExecute,
                )],
            ),
            built_entry(
                0,
                "rootkeeper",
                true,
                0x4000_0000,
                &[built_segment(
                    b"BOOT",
                    0x4000_0000,
                    0x1000,
                    SegmentPermissions::ReadExecute,
                )],
            ),
            built_entry(
                1,
                "logger",
                false,
                0x4800_0000,
                &[built_segment(
                    b"LOG",
                    0x4800_0000,
                    0x1000,
                    SegmentPermissions::ReadExecute,
                )],
            ),
        ];

        let image_bytes = assemble_userland_image(&entries).expect("assemble image");
        let image = decode(&image_bytes).expect("assembled image must parse");
        assert_eq!(image.bootstrap_entry().name(), "rootkeeper");
        assert_eq!(image.entry(1).expect("entry 1").name(), "logger");
        assert_eq!(image.entry(2).expect("entry 2").name(), "shell");
    }

    #[test]
    fn userland_image_layout_is_deterministic() {
        let entries = [
            built_entry(
                1,
                "shell",
                false,
                0x5000_0000,
                &[built_segment(
                    b"ELF",
                    0x5000_0000,
                    0x1000,
                    SegmentPermissions::ReadExecute,
                )],
            ),
            built_entry(
                0,
                "rootkeeper",
                true,
                0x4000_0000,
                &[
                    built_segment(
                        b"CODE",
                        0x4000_0000,
                        0x1000,
                        SegmentPermissions::ReadExecute,
                    ),
                    built_segment(b"RW", 0x4000_1000, 0x1000, SegmentPermissions::ReadWrite),
                ],
            ),
        ];

        let first = assemble_userland_image(&entries).expect("assemble image");
        let second = assemble_userland_image(&entries).expect("assemble image");
        assert_eq!(first, second);
    }

    #[test]
    fn assembled_blob_uses_absolute_blob_offsets() {
        let image_bytes = assemble_userland_image(&[
            built_entry(
                0,
                "rootkeeper",
                true,
                0x4000_0000,
                &[
                    built_segment(
                        b"CODE",
                        0x4000_0000,
                        0x1000,
                        SegmentPermissions::ReadExecute,
                    ),
                    built_segment(b"RW", 0x4000_1000, 0x1000, SegmentPermissions::ReadWrite),
                ],
            ),
            built_entry(
                1,
                "shell",
                false,
                0x5000_0000,
                &[built_segment(
                    b"ELF",
                    0x5000_0000,
                    0x1000,
                    SegmentPermissions::ReadExecute,
                )],
            ),
        ])
        .expect("assemble image");

        let image = decode(&image_bytes).expect("assembled image must parse");
        let bootstrap = image.bootstrap_entry();
        let segments: Vec<_> = bootstrap.segments().collect();
        assert_eq!(segments[0].bytes, b"CODE");
        assert_eq!(segments[1].bytes, b"RW");
    }
}
