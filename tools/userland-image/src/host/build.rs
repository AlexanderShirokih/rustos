use std::{
    ffi::OsStr,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use component_abi::{COMPONENT_MANIFEST_SECTION, decode_manifest, encode_manifest};
use object::{
    Architecture, BinaryFormat, Object, ObjectKind, ObjectSection, ObjectSegment, SectionFlags,
    SegmentFlags, elf,
};
use userland::{Program, Segment, SegmentPermissions, encode_program};

use super::{BuildEnvironment, image::component_record_path, program::ResolvedProgram};

#[derive(Clone, Debug)]
pub(super) struct BuiltEntry {
    pub(super) record_path: String,
    pub(super) program: ResolvedProgram,
    pub(super) bytes: Vec<u8>,
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

    if program.boot {
        let declaration =
            decode_manifest(&manifest).map_err(|error| anyhow!("boot manifest: {error:?}"))?;
        ensure!(
            declaration.requirements().next().is_none(),
            "boot program '{}' declares requirements; a program without a parent takes none",
            program.bin
        );
    }

    let views: Vec<_> = segments
        .iter()
        .map(|segment| Segment {
            bytes: &segment.bytes,
            va_base: segment.va_base,
            mem_size: segment.mem_size,
            permissions: segment.permissions,
        })
        .collect();
    let bytes = encode_program(&Program {
        manifest: &manifest,
        entry_va,
        stack_size: program.stack_size,
        segments: &views,
    })
    .map_err(|error| anyhow!("userland entry '{}' is invalid: {error:?}", program.path))?;
    Ok(BuiltEntry {
        record_path: component_record_path(&program.path),
        program: program.clone(),
        bytes,
    })
}

fn extract_manifest(file: &object::File<'_>, entry: &str) -> Result<Vec<u8>> {
    let Some(section) = file.section_by_name(COMPONENT_MANIFEST_SECTION) else {
        return Ok(encode_manifest::<16>(&[], &[]).to_vec());
    };
    let SectionFlags::Elf { sh_flags } = section.flags() else {
        bail!("userland entry '{entry}' must expose ELF component manifest section flags");
    };
    ensure!(
        sh_flags & u64::from(elf::SHF_ALLOC) == 0,
        "userland entry '{entry}' loads its '{COMPONENT_MANIFEST_SECTION}' section into the image; \
         its linker script must place the section outside the loaded ones"
    );
    if let Some((offset, size)) = section.file_range() {
        ensure!(
            file.segments().all(|segment| {
                let (start, length) = segment.file_range();
                offset
                    .checked_add(size)
                    .zip(start.checked_add(length))
                    .is_some_and(|(end, segment_end)| end <= start || segment_end <= offset)
            }),
            "userland entry '{entry}' repeats its manifest in a PT_LOAD segment"
        );
    }
    let bytes = section.data().with_context(|| {
        format!("Failed to read component manifest section of userland entry '{entry}'")
    })?;
    decode_manifest(bytes).map_err(|error| {
        anyhow!("userland entry '{entry}' carries an invalid component manifest: {error:?}")
    })?;
    Ok(bytes.to_vec())
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

#[cfg(test)]
mod tests {
    use component_abi::{RequirementDeclaration, manifest_len};
    use userland::{ProgramView, decode_program};

    use super::*;

    fn elf_with_manifest(manifest: Option<&[u8]>, flags: u64) -> Vec<u8> {
        let names = b"\0.shstrtab\0.component.manifest\0";
        let mut bytes = vec![0; 516];
        bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        for (at, value) in [
            (16, 2u16),
            (18, 183),
            (52, 64),
            (54, 56),
            (56, 1),
            (58, 64),
            (60, 3),
            (62, 1),
        ] {
            bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
        }
        bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
        for (at, value) in [
            (24, 4096u64),
            (32, 64),
            (40, 120),
            (72, 512),
            (80, 4096),
            (88, 4096),
            (96, 4),
            (104, 4096),
            (112, 4096),
            (208, 312),
            (216, names.len() as u64),
            (272, 384),
            (280, manifest.map_or(0, <[u8]>::len) as u64),
            (256, flags),
        ] {
            bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
        }
        for (at, value) in [(64, 1u32), (68, 5), (184, 1), (188, 3), (252, 1)] {
            bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        if manifest.is_some() {
            bytes[248..252].copy_from_slice(
                &u32::try_from(
                    names
                        .windows(COMPONENT_MANIFEST_SECTION.len())
                        .position(|name| name == COMPONENT_MANIFEST_SECTION.as_bytes())
                        .expect("manifest section name"),
                )
                .expect("name offset fits")
                .to_le_bytes(),
            );
        }
        bytes[312..312 + names.len()].copy_from_slice(names);
        if let Some(manifest) = manifest {
            bytes[384..384 + manifest.len()].copy_from_slice(manifest);
        }
        bytes[512..516].copy_from_slice(b"CODE");
        bytes
    }

    #[test]
    fn section_is_copied_once_even_without_a_plan() {
        const REQUIREMENTS: &[RequirementDeclaration] =
            &[RequirementDeclaration::session("clock", "Clock")];
        const LEN: usize = manifest_len(REQUIREMENTS, &["Display"]);
        let manifest = encode_manifest::<LEN>(REQUIREMENTS, &["Display"]);
        for declaration in [None, Some(manifest.as_slice())] {
            let mut file = tempfile::NamedTempFile::new().expect("ELF file");
            std::io::Write::write_all(&mut file, &elf_with_manifest(declaration, 0))
                .expect("write ELF");
            let program = ResolvedProgram {
                path: String::from("/outside-plan"),
                package: String::from("fixture"),
                bin: String::from("fixture"),
                boot: false,
                stack_size: 4096,
            };
            let entry = read_and_extract_entry(file.path(), &program, Architecture::Aarch64)
                .expect("extract ELF");
            let decoded = decode_program(&entry.bytes).expect("program");
            let expected = encode_manifest::<16>(&[], &[]);
            assert_eq!(decoded.manifest(), declaration.unwrap_or(&expected));
            assert_eq!(decoded.segments().next().expect("segment").bytes, b"CODE");
            if declaration.is_some() {
                assert_eq!(
                    entry
                        .bytes
                        .windows(LEN)
                        .filter(|bytes| *bytes == manifest)
                        .count(),
                    1
                );
            }
        }
    }

    #[test]
    fn invalid_or_loaded_manifest_section_is_rejected() {
        for (manifest, flags) in [
            (b"invalid".as_slice(), 0),
            (&encode_manifest::<16>(&[], &[]), u64::from(elf::SHF_ALLOC)),
        ] {
            let bytes = elf_with_manifest(Some(manifest), flags);
            let file = object::File::parse(bytes.as_slice()).expect("ELF");
            assert!(extract_manifest(&file, "invalid-program").is_err());
        }
    }
}
