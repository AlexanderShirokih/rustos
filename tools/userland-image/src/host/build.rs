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
    Architecture, BinaryFormat, Object, ObjectKind, ObjectSection, ObjectSegment, SectionFlags, elf,
};
use userland::{ProgramView, decode_program};

use super::{BuildEnvironment, image::component_record_path, program::ResolvedProgram};

#[derive(Clone, Debug)]
pub(super) struct BuiltEntry {
    pub(super) record_path: String,
    pub(super) program: ResolvedProgram,
    pub(super) bytes: Vec<u8>,
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

pub(super) fn build_programs(
    userland_root: &Path,
    target_directory: &Path,
    target: &UserlandTarget,
    std_src: &Path,
    programs: &[ResolvedProgram],
    environment: &[BuildEnvironment<'_>],
) -> Result<Vec<BuiltEntry>> {
    let directory_name = target.directory_name()?;

    // Cargo объединяет features всех пакетов одного вызова, а флаги линкера задаёт вызов bin.
    // Поэтому вызов собирает один bin.
    let mut built = Vec::with_capacity(programs.len());
    for program in programs {
        run_userland_build(userland_root, target, std_src, program, environment)?;
        let binary_path = userland_binary_path(target_directory, directory_name, &program.bin);
        built.push(read_and_extract_entry(
            &binary_path,
            program,
            target.architecture,
        )?);
    }

    Ok(built)
}

fn run_userland_build(
    userland_root: &Path,
    target: &UserlandTarget,
    std_src: &Path,
    program: &ResolvedProgram,
    environment: &[BuildEnvironment<'_>],
) -> Result<()> {
    let mut command = Command::new("cargo");
    command.args([
        "rustc",
        "--package",
        &program.package,
        "--release",
        "--target",
    ]);
    command.arg(&target.spec);
    command.args(["--bin", &program.bin]);
    command
        .arg("--")
        .arg("-Clink-arg=-zstack-size=".to_owned() + &program.stack_size.to_string())
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
        .context("Failed to execute cargo build for userland package")?;

    if !status.success() {
        bail!(
            "Failed to build userland package '{}' with status {status}",
            program.package
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
    let manifest = extract_manifest(&file, &program.path)?;

    if program.boot
        && let Some(manifest) = manifest
    {
        let declaration =
            decode_manifest(manifest).map_err(|error| anyhow!("boot manifest: {error:?}"))?;
        ensure!(
            declaration.requirements().next().is_none(),
            "boot program '{}' declares requirements; a program without a parent takes none",
            program.bin
        );
    }

    let decoded = decode_program(&payload)
        .map_err(|error| anyhow!("userland entry '{}' is invalid: {error:?}", program.path))?;
    ensure!(
        decoded.stack_size() == program.stack_size,
        "userland entry '{}' has a {:#x} byte stack, its package asks for {:#x}",
        program.path,
        decoded.stack_size(),
        program.stack_size
    );
    let bytes = payload;

    Ok(BuiltEntry {
        record_path: component_record_path(&program.path),
        program: program.clone(),
        bytes,
    })
}

fn extract_manifest<'data>(file: &object::File<'data>, entry: &str) -> Result<Option<&'data [u8]>> {
    let Some(section) = file.section_by_name(COMPONENT_MANIFEST_SECTION) else {
        return Ok(None);
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

    Ok(Some(bytes))
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

    let debug_section = file
        .sections()
        .filter_map(|section| section.name().ok())
        .find(|name| *name == ".symtab" || name.starts_with(".debug_"));
    if let Some(name) = debug_section {
        bail!(
            "userland entry '{entry}' carries section '{name}'; \
             its package must be built without symbols and debug info"
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use component_abi::encode_manifest;
    use userland::{Program, Segment, SegmentPermissions, encode_elf_program};

    use super::*;

    fn elf(section_name: &str, section: &[u8], stack_size: u64) -> Vec<u8> {
        let mut bytes = encode_elf_program(&Program {
            manifest: section,
            entry_va: 4096,
            stack_size,
            segments: &[Segment {
                va_base: 4096,
                mem_size: 4096,
                permissions: SegmentPermissions::ReadExecute,
                bytes: b"CODE",
            }],
        })
        .expect("fixture ELF");

        let manifest_name = format!("{COMPONENT_MANIFEST_SECTION}\0");
        assert!(section_name.len() < manifest_name.len());
        let name = bytes
            .windows(manifest_name.len())
            .position(|window| window == manifest_name.as_bytes())
            .expect("manifest section name");
        let mut replacement = section_name.as_bytes().to_vec();
        replacement.resize(manifest_name.len(), 0);
        bytes[name..name + manifest_name.len()].copy_from_slice(&replacement);

        bytes
    }

    fn built_entry(bytes: &[u8]) -> Result<BuiltEntry> {
        let mut file = tempfile::NamedTempFile::new()?;
        std::io::Write::write_all(&mut file, bytes)?;
        let program = ResolvedProgram {
            path: String::from("/outside-plan"),
            package: String::from("fixture"),
            bin: String::from("fixture"),
            boot: false,
            stack_size: 4096,
        };
        read_and_extract_entry(file.path(), &program, Architecture::Aarch64)
    }

    #[test]
    fn package_elf_is_stored_without_changes() {
        let manifest = encode_manifest::<16>(&[], &[], &[]);
        let source = elf(COMPONENT_MANIFEST_SECTION, &manifest, 4096);

        let entry = built_entry(&source).expect("extract ELF");
        assert_eq!(entry.bytes, source);
        let decoded = decode_program(&entry.bytes).expect("program");
        assert_eq!(decoded.manifest(), manifest);
        assert_eq!(decoded.segments().next().expect("segment").bytes, b"CODE");
    }

    #[test]
    fn stack_that_differs_from_the_package_is_rejected() {
        let manifest = encode_manifest::<16>(&[], &[], &[]);

        assert!(built_entry(&elf(COMPONENT_MANIFEST_SECTION, &manifest, 8192)).is_err());
    }

    #[test]
    fn invalid_or_loaded_manifest_section_is_rejected() {
        let valid = encode_manifest::<16>(&[], &[], &[]);
        let mut loaded = elf(COMPONENT_MANIFEST_SECTION, &valid, 4096);
        let section = object::File::parse(loaded.as_slice())
            .expect("ELF")
            .section_by_name(COMPONENT_MANIFEST_SECTION)
            .expect("manifest section")
            .index()
            .0;
        let table = usize::try_from(u64::from_le_bytes(
            loaded[40..48].try_into().expect("section table offset"),
        ))
        .expect("offset fits");
        let flags = table + section * 64 + 8;
        loaded[flags..flags + 8].copy_from_slice(&u64::from(elf::SHF_ALLOC).to_le_bytes());

        for bytes in [elf(COMPONENT_MANIFEST_SECTION, b"invalid", 4096), loaded] {
            assert!(built_entry(&bytes).is_err());
        }
    }

    #[test]
    fn symbols_or_debug_info_are_rejected() {
        for name in [".symtab", ".debug_info"] {
            let error = built_entry(&elf(name, b"", 4096)).expect_err("section must be refused");
            assert!(error.to_string().contains(name), "{error}");
        }
    }
}
