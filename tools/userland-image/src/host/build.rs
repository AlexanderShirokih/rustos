use std::{
    ffi::OsStr,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use component_abi::{COMPONENT_MANIFEST_SECTION, decode_manifest};
use object::{
    Architecture, BinaryFormat, Object, ObjectKind, ObjectSection, ObjectSegment, SectionFlags,
    SegmentFlags, elf,
};
use userland::SegmentPermissions;

use super::{BuildEnvironment, image::component_record_path, program::ResolvedProgram};
use crate::manifest::ProgramManifest;

#[derive(Clone, Debug)]
pub(super) struct BuiltEntry {
    pub(super) record_path: String,
    pub(super) program: ResolvedProgram,
    pub(super) entry_va: u64,
    pub(super) segments: Vec<BuiltSegment>,
    pub(super) manifest: ProgramManifest,
}

#[derive(Clone, Debug)]
pub(super) struct BuiltSegment {
    pub(super) bytes: Vec<u8>,
    pub(super) va_base: u64,
    pub(super) mem_size: u64,
    pub(super) permissions: SegmentPermissions,
}

/// Таргет сборки программ образа: спека системного таргета и архитектура, которую обязан
/// нести собранный ELF.
#[derive(Clone, Debug)]
pub struct UserlandTarget {
    pub spec: PathBuf,
    pub architecture: Architecture,
}

impl UserlandTarget {
    /// Таргет по спеке `spec` и имени архитектуры реестра дерева.
    pub fn new(spec: PathBuf, architecture: &str) -> Result<Self> {
        let architecture = match architecture {
            "aarch64" => Architecture::Aarch64,
            "riscv64" => Architecture::Riscv64,
            other => bail!("architecture '{other}' has no ELF machine mapping"),
        };
        Ok(Self { spec, architecture })
    }

    pub(super) fn directory_name(&self) -> Result<&str> {
        self.spec
            .file_stem()
            .and_then(OsStr::to_str)
            .with_context(|| format!("target spec '{}' has no name", self.spec.display()))
    }
}

pub(super) fn build_program(
    userland_root: &Path,
    target_directory: &Path,
    target: &UserlandTarget,
    std_src: &Path,
    program: &ResolvedProgram,
    environment: &[BuildEnvironment<'_>],
) -> Result<BuiltEntry> {
    run_userland_build(userland_root, target, std_src, program, environment)?;
    let binary_path =
        userland_binary_path(target_directory, target.directory_name()?, &program.bin);
    read_and_extract_entry(&binary_path, program, target.architecture)
}

fn run_userland_build(
    userland_root: &Path,
    target: &UserlandTarget,
    std_src: &Path,
    program: &ResolvedProgram,
    environment: &[BuildEnvironment<'_>],
) -> Result<()> {
    let mut command = Command::new("cargo");
    command
        .args([
            "build",
            "--package",
            &program.package,
            "--bin",
            &program.bin,
            "--release",
            "--target",
        ])
        .arg(&target.spec)
        // Cargo читает конфигурацию таргета по рабочему каталогу процесса.
        .current_dir(userland_root)
        .env("__CARGO_TESTS_ONLY_SRC_ROOT", std_src)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    for variable in environment {
        command.env(variable.name, variable.value);
    }
    let status = command
        .status()
        .context("Failed to execute cargo build for userland entry")?;

    if !status.success() {
        bail!(
            "Failed to build userland entry '{}' with status {}",
            program.path,
            status
        );
    }

    Ok(())
}

pub(super) fn userland_binary_path(target_directory: &Path, target: &str, bin: &str) -> PathBuf {
    target_directory.join(target).join("release").join(bin)
}

fn read_and_extract_entry(
    path: &Path,
    program: &ResolvedProgram,
    architecture: Architecture,
) -> Result<BuiltEntry> {
    let mut payload = Vec::new();
    File::open(path)
        .with_context(|| format!("Failed to open userland ELF '{}'", path.display()))?
        .read_to_end(&mut payload)
        .with_context(|| format!("Failed to read userland ELF '{}'", path.display()))?;

    ensure!(
        path.extension() != Some(OsStr::new("d")),
        "unexpected dep-info path for userland entry '{}'",
        program.path
    );

    let file = object::File::parse(payload.as_slice()).with_context(|| {
        format!(
            "Failed to parse payload ELF for userland entry '{}'",
            program.path
        )
    })?;

    validate_file(&file, &program.path, architecture)?;
    let entry_va = file.entry();
    let segments = extract_segments(&file, &program.path)?;
    let manifest = extract_manifest(&file, &program.path)?;

    Ok(BuiltEntry {
        record_path: component_record_path(&program.path),
        program: program.clone(),
        entry_va,
        segments,
        manifest,
    })
}

fn extract_manifest(file: &object::File<'_>, entry: &str) -> Result<ProgramManifest> {
    // Секции нет у программы, которой объявлять нечего: её манифест пуст.
    let Some(section) = file.section_by_name(COMPONENT_MANIFEST_SECTION) else {
        return Ok(ProgramManifest::default());
    };
    let SectionFlags::Elf { sh_flags } = section.flags() else {
        bail!("userland entry '{entry}' must expose ELF component manifest section flags");
    };
    ensure!(
        sh_flags & u64::from(elf::SHF_ALLOC) == 0,
        "userland entry '{entry}' loads its '{COMPONENT_MANIFEST_SECTION}' section into the image; \
         its linker script must place the section outside the loaded ones"
    );
    let bytes = section.data().with_context(|| {
        format!("Failed to read component manifest section of userland entry '{entry}'")
    })?;
    let manifest = decode_manifest(bytes).map_err(|error| {
        anyhow!("userland entry '{entry}' carries an invalid component manifest: {error:?}")
    })?;
    Ok(ProgramManifest::from_record(&manifest))
}

fn validate_file(file: &object::File<'_>, entry: &str, architecture: Architecture) -> Result<()> {
    ensure!(
        file.format() == BinaryFormat::Elf,
        "userland entry '{entry}' payload is not ELF"
    );
    ensure!(
        file.architecture() == architecture,
        "userland entry '{entry}' payload must target {architecture:?}, found {:?}",
        file.architecture()
    );
    ensure!(
        file.kind() == ObjectKind::Executable,
        "userland entry '{entry}' payload must be ET_EXEC"
    );
    ensure!(
        file.is_little_endian(),
        "userland entry '{entry}' payload must be little-endian"
    );
    ensure!(
        file.is_64(),
        "userland entry '{entry}' payload must be ELF64"
    );
    Ok(())
}

fn extract_segments(file: &object::File<'_>, entry: &str) -> Result<Vec<BuiltSegment>> {
    let mut segments = Vec::new();

    for segment in file.segments() {
        let bytes = segment
            .data()
            .with_context(|| format!("Failed to read PT_LOAD bytes for '{entry}'"))?;
        let permissions = map_segment_permissions(segment.flags(), entry)?;
        segments.push(BuiltSegment {
            bytes: bytes.to_vec(),
            va_base: segment.address(),
            mem_size: segment.size(),
            permissions,
        });
    }

    ensure!(
        !segments.is_empty(),
        "userland entry '{entry}' must contain at least one PT_LOAD segment"
    );

    Ok(segments)
}

fn map_segment_permissions(flags: SegmentFlags, entry: &str) -> Result<SegmentPermissions> {
    let SegmentFlags::Elf { p_flags } = flags else {
        bail!("userland entry '{entry}' must expose ELF PT_LOAD segment flags");
    };

    let readable = (p_flags & elf::PF_R) != 0;
    let writable = (p_flags & elf::PF_W) != 0;
    let executable = (p_flags & elf::PF_X) != 0;

    match (readable, writable, executable) {
        (true, true, false) => Ok(SegmentPermissions::ReadWrite),
        (true, false, false) => Ok(SegmentPermissions::ReadOnly),
        (true, false, true) => Ok(SegmentPermissions::ReadExecute),
        _ => bail!("userland entry '{entry}' has unsupported PT_LOAD flags {p_flags:#x}"),
    }
}
