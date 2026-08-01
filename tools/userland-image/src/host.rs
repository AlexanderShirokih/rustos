use std::{
    collections::HashMap,
    ffi::OsStr,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    str::FromStr,
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use filesystem_abi::{NAME_MAX, PATH_MAX};
use object::{Architecture, BinaryFormat, Object, ObjectKind, ObjectSegment, SegmentFlags, elf};
use serde::Deserialize;
use userland::{
    Entry, Segment, SegmentPermissions, USERLAND_ENTRY_PATH_CAPACITY, USERLAND_PAGE_SIZE,
};
use userland_image::{ImageRecord, USERLAND_IMAGE_VERSION, decode, encode, encode_program};

const USERLAND_MANIFEST_VERSION: u16 = USERLAND_IMAGE_VERSION;
const USERLAND_STACK_ALIGN: u64 = USERLAND_PAGE_SIZE;

const WILDCARDS: [char; 2] = ['*', '?'];

// Путь записи образа - путь контракта: граница длины у них общая.
const _: () = assert!(PATH_MAX == USERLAND_ENTRY_PATH_CAPACITY);

/// Переменная окружения, передаваемая сборке каждой программы образа.
#[derive(Clone, Copy, Debug)]
pub struct BuildEnvironment<'a> {
    pub name: &'a str,
    pub value: &'a str,
}

/// Файл, добавляемый в образ поверх манифеста: путь записи и источник на диске
/// относительно корня проекта. Разбирается из аргумента вида `path=source`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtraFile {
    pub path: String,
    pub source: PathBuf,
}

// Композиция образа: версия, программы и файлы, попадающие в таблицу записей.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImageManifest {
    #[serde(default = "default_manifest_version")]
    version: u16,
    programs: Vec<ImageProgram>,
    #[serde(default)]
    files: Vec<ImageFile>,
}

// Программа: путь в образе, пакет workspace и точечные override метаданных.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImageProgram {
    package: String,
    path: String,
    bin: Option<String>,
    boot: Option<bool>,
    stack_size: Option<u64>,
}

// Файл: путь в образе и источник на диске. Источник без шаблона даёт одну
// запись, источник с шаблоном - дерево под путём записи.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImageFile {
    path: String,
    source: PathBuf,
}

/// Свойства программы из секции `[package.metadata.userland]` её Cargo.toml.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct UserlandMetadata {
    bin: Option<String>,
    stack_size: Option<u64>,
}

/// Программа после слияния метаданных пакета и override образа.
#[derive(Clone, Debug)]
struct ResolvedProgram {
    path: String,
    package: String,
    bin: String,
    boot: bool,
    stack_size: u64,
}

#[derive(Debug, Deserialize)]
struct CargoMetadataOutput {
    packages: Vec<CargoPackage>,
    target_directory: PathBuf,
}

#[derive(Debug, Deserialize)]
struct CargoPackage {
    name: String,
    #[serde(default)]
    metadata: Option<serde_json::Value>,
}

struct WorkspaceMetadata {
    packages: HashMap<String, UserlandMetadata>,
    target_directory: PathBuf,
}

#[derive(Clone, Debug)]
struct BuiltEntry {
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

#[derive(Clone, Debug)]
struct LoadedFile {
    path: String,
    source: PathBuf,
    bytes: Vec<u8>,
}

#[derive(Clone, Debug)]
struct DirectoryFile {
    relative: String,
    source: PathBuf,
}

// Источник с подстановкой: корень обхода на диске и шаблон отбора его файлов.
struct SourcePattern<'a> {
    root: &'a str,
    glob: &'a str,
}

// Повтор пути с тем же содержимым сливается в одну запись, с другим -
// отвергается.
#[derive(Debug, Default)]
struct FileSet {
    files: Vec<LoadedFile>,
    positions: HashMap<String, usize>,
}

impl FromStr for ExtraFile {
    type Err = anyhow::Error;

    fn from_str(argument: &str) -> Result<Self> {
        let (path, source) = argument
            .split_once('=')
            .ok_or_else(|| anyhow!("image file '{argument}' must be given as 'path=source'"))?;
        ensure!(
            !path.is_empty() && !source.is_empty(),
            "image file '{argument}' must set both a non-empty path and a non-empty source"
        );
        Ok(Self {
            path: path.to_string(),
            source: PathBuf::from(source),
        })
    }
}

impl FileSet {
    fn insert(&mut self, file: LoadedFile) -> Result<()> {
        if let Some(&position) = self.positions.get(&file.path) {
            let present = &self.files[position];
            ensure!(
                present.bytes == file.bytes,
                "image path '{}' is filled by '{}' and '{}' with different content",
                file.path,
                present.source.display(),
                file.source.display()
            );
            return Ok(());
        }
        self.positions.insert(file.path.clone(), self.files.len());
        self.files.push(file);
        Ok(())
    }

    fn into_files(self) -> Vec<LoadedFile> {
        self.files
    }
}

fn default_manifest_version() -> u16 {
    USERLAND_MANIFEST_VERSION
}

/// Собирает userland-образ, передавая `environment` каждому cargo-процессу
/// сборки. `extra_files` добавляются в образ поверх файлов манифеста.
pub fn build_userland(
    project_root: &Path,
    manifest_path: &Path,
    build_dir: &Path,
    environment: &[BuildEnvironment<'_>],
    extra_files: &[ExtraFile],
) -> Result<PathBuf> {
    let manifest = load_manifest(manifest_path)?;
    let workspace = load_workspace_metadata(project_root)?;
    let programs = resolve_programs(&manifest, &workspace.packages)?;
    let files = collect_files(project_root, &manifest, extra_files)?;
    let built_entries = programs
        .iter()
        .map(|program| {
            build_program(
                project_root,
                &workspace.target_directory,
                program,
                environment,
            )
        })
        .collect::<Result<Vec<_>>>()?;

    let blob = assemble_userland_image(&built_entries, &files)?;
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

// Файлы образа собираются в одном порядке: файлы манифеста, затем файлы
// командной строки.
fn collect_files(
    project_root: &Path,
    manifest: &ImageManifest,
    extra_files: &[ExtraFile],
) -> Result<Vec<LoadedFile>> {
    let mut files = FileSet::default();
    for file in &manifest.files {
        for loaded in load_source(&file.path, project_root, &file.source)? {
            files.insert(loaded)?;
        }
    }
    for file in extra_files {
        for loaded in load_source(&file.path, project_root, &file.source)? {
            files.insert(loaded)?;
        }
    }
    Ok(files.into_files())
}

// Источник без шаблона даёт запись по пути `path`, источник с шаблоном - дерево
// под ним, где путь записи наращивается путём файла от корня обхода.
fn load_source(path: &str, project_root: &Path, source: &Path) -> Result<Vec<LoadedFile>> {
    validate_image_path(path)?;
    let text = source
        .to_str()
        .ok_or_else(|| anyhow!("image source '{}' is not UTF-8", source.display()))?;
    let Some(pattern) = split_pattern(text) else {
        let source = project_root.join(source);
        ensure!(
            !source.is_dir(),
            "image source '{}' of path '{path}' is a directory; \
             its files are selected by a pattern such as '{text}/**'",
            source.display()
        );
        return Ok(vec![load_file(path, source)?]);
    };

    // Корень образа префикса не даёт, остальные пути дают себя целиком.
    let prefix = path.trim_end_matches('/');
    let mut found = Vec::new();
    collect_directory_files(&project_root.join(pattern.root), "", &mut found)?;
    found
        .into_iter()
        .filter(|file| glob_matches(pattern.glob, &file.relative))
        .map(|file| load_file(&format!("{prefix}/{}", file.relative), file.source))
        .collect()
}

// Источник делится по первому компоненту с подстановкой: перед ним корень
// обхода, с него начинается шаблон.
fn split_pattern(source: &str) -> Option<SourcePattern<'_>> {
    let mut cursor = 0;
    for component in source.split('/') {
        if component.contains(WILDCARDS) {
            return Some(SourcePattern {
                root: source[..cursor].trim_end_matches('/'),
                glob: &source[cursor..],
            });
        }
        cursor += component.len() + 1;
    }
    None
}

fn load_file(path: &str, source: PathBuf) -> Result<LoadedFile> {
    validate_image_path(path)?;
    let bytes = fs::read(&source).with_context(|| {
        format!(
            "Failed to read source of image file '{}' from '{}'",
            path,
            source.display()
        )
    })?;
    Ok(LoadedFile {
        path: path.to_string(),
        source,
        bytes,
    })
}

// Записи каталога обходятся в порядке имени: состав образа не зависит от порядка
// выдачи файловой системы.
fn collect_directory_files(
    directory: &Path,
    prefix: &str,
    found: &mut Vec<DirectoryFile>,
) -> Result<()> {
    let read_dir = fs::read_dir(directory)
        .with_context(|| format!("Failed to read image directory '{}'", directory.display()))?;
    let mut entries = read_dir
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("Failed to read image directory '{}'", directory.display()))?;
    entries.sort_by_key(std::fs::DirEntry::file_name);

    for entry in entries {
        let source = entry.path();
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow!("image directory entry '{}' is not UTF-8", source.display()))?;
        let relative = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let file_type = entry.file_type().with_context(|| {
            format!("Failed to read type of image entry '{}'", source.display())
        })?;

        if file_type.is_dir() {
            collect_directory_files(&source, &relative, found)?;
        } else if file_type.is_file() {
            found.push(DirectoryFile { relative, source });
        } else {
            bail!(
                "image directory entry '{}' is neither a file nor a directory",
                source.display()
            );
        }
    }
    Ok(())
}

// `?` - один символ, `*` - их последовательность внутри компонента, `**` -
// вместе с разделителями, ведущий `**/` - и путь без каталогов.
fn glob_matches(glob: &str, path: &str) -> bool {
    matches_bytes(glob.as_bytes(), path.as_bytes())
}

fn matches_bytes(glob: &[u8], path: &[u8]) -> bool {
    match glob.first() {
        None => path.is_empty(),
        Some(b'*') if glob.get(1) == Some(&b'*') => {
            let tail = &glob[2..];
            if tail.first() == Some(&b'/') && matches_bytes(&tail[1..], path) {
                return true;
            }
            (0..=path.len()).any(|split| matches_bytes(tail, &path[split..]))
        }
        Some(b'*') => {
            let component = path
                .iter()
                .position(|byte| *byte == b'/')
                .unwrap_or(path.len());
            (0..=component).any(|split| matches_bytes(&glob[1..], &path[split..]))
        }
        Some(b'?') => {
            matches!(path.first(), Some(byte) if *byte != b'/')
                && matches_bytes(&glob[1..], &path[1..])
        }
        Some(expected) => path.first() == Some(expected) && matches_bytes(&glob[1..], &path[1..]),
    }
}

fn load_workspace_metadata(project_root: &Path) -> Result<WorkspaceMetadata> {
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
    let packages = collect_userland_metadata(parsed.packages)?;
    Ok(WorkspaceMetadata {
        packages,
        target_directory: parsed.target_directory,
    })
}

fn parse_cargo_metadata(json: &[u8]) -> Result<CargoMetadataOutput> {
    serde_json::from_slice(json).context("Failed to parse cargo metadata JSON")
}

fn collect_userland_metadata(
    packages: Vec<CargoPackage>,
) -> Result<HashMap<String, UserlandMetadata>> {
    packages
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
        }
    }

    match resolved.iter().filter(|program| program.boot).count() {
        1 => {}
        0 => bail!("userland image must contain exactly one boot program; found none"),
        count => bail!("userland image must contain exactly one boot program; found {count}"),
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
        path: program.path.clone(),
        package: program.package.clone(),
        bin: program
            .bin
            .clone()
            .or_else(|| meta.bin.clone())
            .unwrap_or_else(|| program.package.clone()),
        boot: program.boot.unwrap_or(false),
        stack_size,
    };
    validate_program(&resolved)?;
    Ok(resolved)
}

// Правила пути контракта (filesystem_abi::Path) одни на программы и файлы, включая
// ASCII-ограничение записи образа.
fn validate_image_path(path: &str) -> Result<()> {
    filesystem_abi::Path::new(path).map_err(|_| {
        anyhow!(
            "image path '{path}' must be absolute and at most {PATH_MAX} bytes, \
             with components of 1..={NAME_MAX} bytes other than '.' and '..'"
        )
    })?;
    ensure!(path.is_ascii(), "image path '{path}' must be ASCII");
    Ok(())
}

fn validate_program(program: &ResolvedProgram) -> Result<()> {
    validate_image_path(&program.path)?;
    ensure!(
        !program.bin.is_empty(),
        "userland entry '{}' bin must not be empty",
        program.path
    );
    ensure!(
        program.stack_size != 0 && program.stack_size.is_multiple_of(USERLAND_STACK_ALIGN),
        "userland entry '{}' stack_size must be a non-zero multiple of {} bytes; got {}",
        program.path,
        USERLAND_STACK_ALIGN,
        program.stack_size
    );
    Ok(())
}

fn build_program(
    project_root: &Path,
    target_directory: &Path,
    program: &ResolvedProgram,
    environment: &[BuildEnvironment<'_>],
) -> Result<BuiltEntry> {
    run_userland_build(project_root, program, environment)?;
    let binary_path = userland_binary_path(target_directory, &program.bin);
    read_and_extract_entry(&binary_path, program)
}

fn run_userland_build(
    project_root: &Path,
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
            "--target",
            "aarch64-unknown-none",
            "--release",
        ])
        .current_dir(project_root)
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

fn userland_binary_path(target_directory: &Path, bin: &str) -> PathBuf {
    target_directory
        .join("aarch64-unknown-none/release")
        .join(bin)
}

fn read_and_extract_entry(path: &Path, program: &ResolvedProgram) -> Result<BuiltEntry> {
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

    validate_file(&file, program)?;
    let entry_va = file.entry();
    let segments = extract_segments(&file, program)?;

    Ok(BuiltEntry {
        program: program.clone(),
        entry_va,
        segments,
    })
}

fn validate_file(file: &object::File<'_>, program: &ResolvedProgram) -> Result<()> {
    ensure!(
        file.format() == BinaryFormat::Elf,
        "userland entry '{}' payload is not ELF",
        program.path
    );
    ensure!(
        file.architecture() == Architecture::Aarch64,
        "userland entry '{}' payload must target AArch64",
        program.path
    );
    ensure!(
        file.kind() == ObjectKind::Executable,
        "userland entry '{}' payload must be ET_EXEC",
        program.path
    );
    ensure!(
        file.is_little_endian(),
        "userland entry '{}' payload must be little-endian",
        program.path
    );
    ensure!(
        file.is_64(),
        "userland entry '{}' payload must be ELF64",
        program.path
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
            .with_context(|| format!("Failed to read PT_LOAD bytes for '{}'", program.path))?;
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
        program.path
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
            program.path
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
            program.path
        ),
    }
}

// Собранный образ разбирается обратно как проверка сборки.
fn assemble_userland_image(entries: &[BuiltEntry], files: &[LoadedFile]) -> Result<Vec<u8>> {
    let boot_path = validate_built_entries(entries, files)?;

    let blobs = entries
        .iter()
        .map(build_program_blob)
        .collect::<Result<Vec<_>>>()?;
    let records: Vec<ImageRecord<'_>> = entries
        .iter()
        .zip(&blobs)
        .map(|(entry, blob)| ImageRecord {
            path: &entry.program.path,
            data: blob,
        })
        .chain(files.iter().map(|file| ImageRecord {
            path: &file.path,
            data: &file.bytes,
        }))
        .collect();

    let image = encode(&records, boot_path)
        .map_err(|err| anyhow!("failed to serialize userland image: {err:?}"))?;

    let decoded =
        decode(&image).map_err(|err| anyhow!("assembled userland image is invalid: {err:?}"))?;
    decoded
        .boot_entry()
        .program()
        .map_err(|err| anyhow!("boot record '{boot_path}' is not a program: {err:?}"))?;
    Ok(image)
}

fn build_program_blob(entry: &BuiltEntry) -> Result<Vec<u8>> {
    let segments: Vec<Segment<'_>> = entry
        .segments
        .iter()
        .map(|segment| Segment {
            va_base: segment.va_base,
            mem_size: segment.mem_size,
            permissions: segment.permissions,
            bytes: &segment.bytes,
        })
        .collect();

    encode_program(&Entry {
        path: &entry.program.path,
        entry_va: entry.entry_va,
        stack_size: entry.program.stack_size,
        segments: &segments,
    })
    .map_err(|err| {
        anyhow!(
            "failed to serialize program '{}': {err:?}",
            entry.program.path
        )
    })
}

fn validate_built_entries<'a>(entries: &'a [BuiltEntry], files: &[LoadedFile]) -> Result<&'a str> {
    ensure!(
        !entries.is_empty(),
        "userland image must contain at least one entry"
    );

    for entry in entries {
        validate_program(&entry.program)?;
        ensure!(
            !entry.segments.is_empty(),
            "userland entry '{}' must contain at least one segment",
            entry.program.path
        );
    }

    let paths = entries
        .iter()
        .map(|entry| entry.program.path.as_str())
        .chain(files.iter().map(|file| file.path.as_str()));
    let mut seen = Vec::new();
    for path in paths {
        validate_image_path(path)?;
        ensure!(
            !seen.contains(&path),
            "image path '{path}' is used by more than one record"
        );
        seen.push(path);
    }

    let boot_entries = entries
        .iter()
        .filter(|entry| entry.program.boot)
        .collect::<Vec<_>>();
    match boot_entries.len() {
        1 => {
            let boot = boot_entries[0];
            ensure!(
                boot.segments
                    .iter()
                    .any(|segment| !segment.bytes.is_empty()),
                "boot entry '{}' payload must not be empty",
                boot.program.path
            );
            Ok(&boot.program.path)
        }
        0 => bail!("userland image must contain exactly one boot entry; found none"),
        count => bail!("userland image must contain exactly one boot entry; found {count}"),
    }
}

#[cfg(test)]
mod tests {
    use userland::EntryView;

    use super::*;

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

    fn package_metadata(stack_size: u64) -> UserlandMetadata {
        UserlandMetadata {
            bin: None,
            stack_size: Some(stack_size),
        }
    }

    fn metadata_map(packages: &[(&str, u64)]) -> HashMap<String, UserlandMetadata> {
        packages
            .iter()
            .map(|(package, stack_size)| ((*package).to_string(), package_metadata(*stack_size)))
            .collect()
    }

    fn manifest_of(programs: &[(&str, &str, bool)]) -> ImageManifest {
        ImageManifest {
            version: USERLAND_MANIFEST_VERSION,
            programs: programs
                .iter()
                .map(|(package, path, boot)| image_program(package, path, *boot))
                .collect(),
            files: Vec::new(),
        }
    }

    fn resolved_program(package: &str, path: &str, boot: bool, stack_size: u64) -> ResolvedProgram {
        ResolvedProgram {
            path: path.to_string(),
            package: package.to_string(),
            bin: package.to_string(),
            boot,
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

    fn built_entry(path: &str, boot: bool, entry_va: u64, segments: &[BuiltSegment]) -> BuiltEntry {
        BuiltEntry {
            program: resolved_program("package", path, boot, 0x4000),
            entry_va,
            segments: segments.to_vec(),
        }
    }

    fn image_file(path: &str, source: &str) -> ImageFile {
        ImageFile {
            path: path.to_string(),
            source: PathBuf::from(source),
        }
    }

    fn manifest_with_file(path: &str, source: &str) -> ImageManifest {
        let mut manifest = manifest_of(&[("rootkeeper", BOOT_PATH, true)]);
        manifest.files.push(image_file(path, source));
        manifest
    }

    // Дерево на диске под тест: прежнее содержимое каталога снимается перед раскладкой.
    fn source_tree(name: &str, files: &[(&str, &[u8])]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("userland-image-{name}"));
        drop(fs::remove_dir_all(&root));
        for (path, bytes) in files {
            let path = root.join(path);
            fs::create_dir_all(path.parent().expect("source path has a parent"))
                .expect("create source directories");
            fs::write(path, bytes).expect("write source file");
        }
        root
    }

    fn collected_paths(files: &[LoadedFile]) -> Vec<&str> {
        files.iter().map(|file| file.path.as_str()).collect()
    }

    fn loaded_file(path: &str, bytes: &[u8]) -> LoadedFile {
        LoadedFile {
            path: path.to_string(),
            source: PathBuf::from("source"),
            bytes: bytes.to_vec(),
        }
    }

    #[test]
    fn manifest_parses_programs_and_files() {
        let manifest = parse_manifest(
            "version = 1\n\
             \n\
             [[programs]]\n\
             package = \"rootkeeper\"\n\
             path = \"/boot/rootkeeper\"\n\
             boot = true\n\
             \n\
             [[programs]]\n\
             package = \"testrunner\"\n\
             path = \"/boot/tests\"\n\
             stack_size = 131072\n\
             \n\
             [[files]]\n\
             path = \"/data/sample\"\n\
             source = \"user/rootkeeper/sample.txt\"\n",
        )
        .expect("composition manifest must parse");

        assert_eq!(manifest.version, 1);
        assert_eq!(manifest.programs.len(), 2);
        assert_eq!(manifest.programs[0].path, "/boot/rootkeeper");
        assert_eq!(manifest.programs[0].boot, Some(true));
        assert_eq!(manifest.programs[1].stack_size, Some(131_072));
        assert_eq!(manifest.files[0].path, "/data/sample");
        assert_eq!(
            manifest.files[0].source,
            PathBuf::from("user/rootkeeper/sample.txt")
        );
    }

    #[test]
    fn manifest_parses_a_source_pattern() {
        let manifest = parse_manifest(
            "version = 1\n\
             \n\
             [[programs]]\n\
             package = \"rootkeeper\"\n\
             path = \"/boot/rootkeeper\"\n\
             boot = true\n\
             \n\
             [[files]]\n\
             path = \"/data/assets\"\n\
             source = \"user/rootkeeper/assets/**/*.txt\"\n",
        )
        .expect("composition manifest must parse");

        assert_eq!(manifest.files[0].path, "/data/assets");
        assert_eq!(
            manifest.files[0].source,
            PathBuf::from("user/rootkeeper/assets/**/*.txt")
        );
    }

    #[test]
    fn manifest_rejects_program_without_path() {
        let err = parse_manifest("version = 1\n\n[[programs]]\npackage = \"rootkeeper\"\n")
            .expect_err("path is required");
        assert!(
            err.to_string()
                .contains("Failed to parse userland image manifest TOML")
        );
    }

    #[test]
    fn manifest_rejects_entry_name_override() {
        let err = parse_manifest(
            "version = 1\n\
             \n\
             [[programs]]\n\
             package = \"rootkeeper\"\n\
             path = \"/boot/rootkeeper\"\n\
             name = \"rootkeeper\"\n",
        )
        .expect_err("name override must fail");
        assert!(
            err.to_string()
                .contains("Failed to parse userland image manifest TOML")
        );
    }

    #[test]
    fn manifest_rejects_unsupported_version() {
        let err = parse_manifest(
            "version = 2\n\n[[programs]]\npackage = \"rootkeeper\"\npath = \"/boot/rootkeeper\"\n",
        )
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
        let manifest = manifest_of(&[
            ("rootkeeper", BOOT_PATH, true),
            ("logger", "/boot/logger", false),
        ]);
        let metadata = metadata_map(&[("rootkeeper", 0x10000), ("logger", 0x4000)]);

        let programs = resolve_programs(&manifest, &metadata).expect("resolve must succeed");
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
        let mut manifest = manifest_of(&[("rootkeeper", BOOT_PATH, true)]);
        manifest.programs[0].bin = Some("rootkeeper-min".to_string());
        manifest.programs[0].stack_size = Some(0x8000);
        let metadata = metadata_map(&[("rootkeeper", 0x10000)]);

        let programs = resolve_programs(&manifest, &metadata).expect("resolve must succeed");
        assert_eq!(programs[0].bin, "rootkeeper-min");
        assert_eq!(programs[0].stack_size, 0x8000);
    }

    #[test]
    fn resolve_rejects_package_missing_from_workspace() {
        let manifest = manifest_of(&[("ghost", "/boot/ghost", true)]);
        let metadata = metadata_map(&[("rootkeeper", 0x10000)]);

        let err = resolve_programs(&manifest, &metadata).expect_err("unknown package must fail");
        assert_eq!(
            err.to_string(),
            "package 'ghost' from image manifest not found in workspace"
        );
    }

    #[test]
    fn resolve_rejects_missing_stack_size() {
        let manifest = manifest_of(&[("rootkeeper", BOOT_PATH, true)]);
        let mut metadata = metadata_map(&[("rootkeeper", 0x10000)]);
        metadata.get_mut("rootkeeper").unwrap().stack_size = None;

        let err = resolve_programs(&manifest, &metadata).expect_err("missing stack_size must fail");
        assert!(
            err.to_string()
                .contains("package 'rootkeeper' is missing required field 'stack_size'")
        );
    }

    #[test]
    fn resolve_rejects_bad_stack_size() {
        let manifest = manifest_of(&[("rootkeeper", BOOT_PATH, true)]);
        let metadata = metadata_map(&[("rootkeeper", 123)]);

        let err = resolve_programs(&manifest, &metadata).expect_err("bad stack_size must fail");
        assert!(
            err.to_string()
                .contains("stack_size must be a non-zero multiple of 4096 bytes; got 123")
        );
    }

    #[test]
    fn resolve_rejects_paths_outside_the_contract() {
        let metadata = metadata_map(&[("rootkeeper", 0x4000)]);
        let rejected = |path: &str| {
            let manifest = manifest_of(&[("rootkeeper", path, true)]);
            resolve_programs(&manifest, &metadata)
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
    fn assemble_rejects_a_file_path_outside_the_contract() {
        let entries = [built_entry(
            BOOT_PATH,
            true,
            0x4000_0000,
            &[built_segment(
                b"BOOT",
                0x4000_0000,
                0x1000,
                SegmentPermissions::ReadExecute,
            )],
        )];
        let err = assemble_userland_image(&entries, &[loaded_file("/data/./sample", b"hi")])
            .expect_err("path outside the contract must fail");
        assert!(err.to_string().starts_with("image path '/data/./sample'"));
    }

    #[test]
    fn assemble_rejects_a_non_ascii_file_path() {
        let entries = [built_entry(
            BOOT_PATH,
            true,
            0x4000_0000,
            &[built_segment(
                b"BOOT",
                0x4000_0000,
                0x1000,
                SegmentPermissions::ReadExecute,
            )],
        )];
        // Правило ASCII одно на все записи образа, не только на программы.
        let err = assemble_userland_image(&entries, &[loaded_file("/data/мотд", b"hi")])
            .expect_err("non-ascii path must fail");
        assert_eq!(err.to_string(), "image path '/data/мотд' must be ASCII");
    }

    #[test]
    fn resolve_rejects_zero_boot_programs() {
        let manifest = manifest_of(&[
            ("rootkeeper", BOOT_PATH, false),
            ("shell", "/boot/shell", false),
        ]);
        let metadata = metadata_map(&[("rootkeeper", 0x4000), ("shell", 0x4000)]);

        let err = resolve_programs(&manifest, &metadata).expect_err("no boot must fail");
        assert_eq!(
            err.to_string(),
            "userland image must contain exactly one boot program; found none"
        );
    }

    #[test]
    fn resolve_rejects_multiple_boot_programs() {
        let manifest = manifest_of(&[
            ("rootkeeper", BOOT_PATH, true),
            ("shell", "/boot/shell", true),
        ]);
        let metadata = metadata_map(&[("rootkeeper", 0x4000), ("shell", 0x4000)]);

        let err = resolve_programs(&manifest, &metadata).expect_err("two boot programs must fail");
        assert_eq!(
            err.to_string(),
            "userland image must contain exactly one boot program; found 2"
        );
    }

    #[test]
    fn resolve_rejects_duplicate_package() {
        let manifest = manifest_of(&[
            ("rootkeeper", BOOT_PATH, true),
            ("rootkeeper", "/boot/second", false),
        ]);
        let metadata = metadata_map(&[("rootkeeper", 0x4000)]);

        let err = resolve_programs(&manifest, &metadata).expect_err("duplicate package must fail");
        assert_eq!(
            err.to_string(),
            "binary 'rootkeeper::rootkeeper' is listed more than once in image manifest"
        );
    }

    #[test]
    fn resolve_allows_same_package_distinct_bins() {
        let mut manifest = manifest_of(&[
            ("testrunner", "/boot/testrunner", true),
            ("testrunner", "/boot/fixture", false),
        ]);
        manifest.programs[1].bin = Some("fixture".to_string());
        let metadata = metadata_map(&[("testrunner", 0x4000)]);

        let resolved = resolve_programs(&manifest, &metadata).expect("distinct bins allowed");
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[1].bin, "fixture");
        assert!(!resolved[1].boot);
    }

    #[test]
    fn workspace_metadata_reads_userland_section() {
        let json = br#"{"target_directory": "/workspace/target", "packages": [
            {"name": "rootkeeper",
             "metadata": {"userland": {"bin": "rootkeeper", "stack_size": 65536}}},
            {"name": "memory", "metadata": null}
        ]}"#;

        let parsed = parse_cargo_metadata(json).expect("cargo metadata must parse");
        assert_eq!(parsed.target_directory, PathBuf::from("/workspace/target"));
        let metadata =
            collect_userland_metadata(parsed.packages).expect("userland metadata must parse");
        let rootkeeper = &metadata["rootkeeper"];
        assert_eq!(rootkeeper.bin.as_deref(), Some("rootkeeper"));
        assert_eq!(rootkeeper.stack_size, Some(65536));

        let memory = &metadata["memory"];
        assert_eq!(memory.bin, None);
        assert_eq!(memory.stack_size, None);
    }

    #[test]
    fn workspace_metadata_rejects_unknown_userland_field() {
        let json = br#"{"target_directory": "/workspace/target", "packages": [
            {"name": "rootkeeper",
             "metadata": {"userland": {"bootstrap": true, "stack_size": 65536}}}
        ]}"#;

        let parsed = parse_cargo_metadata(json).expect("cargo metadata must parse");
        let err = collect_userland_metadata(parsed.packages).expect_err("unknown field must fail");
        assert!(
            err.to_string()
                .contains("Invalid [package.metadata.userland] in package 'rootkeeper'")
        );
    }

    #[test]
    fn boot_entry_must_not_be_empty() {
        let entries = [built_entry(
            BOOT_PATH,
            true,
            0x4000_0000,
            &[built_segment(
                &[],
                0x4000_0000,
                0x1000,
                SegmentPermissions::ReadExecute,
            )],
        )];
        let err = assemble_userland_image(&entries, &[]).expect_err("empty boot must fail");
        assert_eq!(
            err.to_string(),
            "boot entry '/boot/rootkeeper' payload must not be empty"
        );
    }

    #[test]
    fn assemble_rejects_duplicate_paths() {
        let entries = [built_entry(
            BOOT_PATH,
            true,
            0x4000_0000,
            &[built_segment(
                b"BOOT",
                0x4000_0000,
                0x1000,
                SegmentPermissions::ReadExecute,
            )],
        )];
        let err = assemble_userland_image(&entries, &[loaded_file(BOOT_PATH, b"clash")])
            .expect_err("duplicate path must fail");
        assert_eq!(
            err.to_string(),
            "image path '/boot/rootkeeper' is used by more than one record"
        );
    }

    #[test]
    fn boot_index_follows_the_boot_path_after_sorting() {
        let entries = [
            built_entry(
                "/boot/shell",
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
                BOOT_PATH,
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
                "/boot/logger",
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

        let image_bytes = assemble_userland_image(&entries, &[loaded_file("/data/sample", b"hi")])
            .expect("assemble image");
        let image = decode(&image_bytes).expect("assembled image must parse");

        let paths: Vec<_> = image.entries().map(|entry| entry.path()).collect();
        assert_eq!(
            paths,
            ["/boot/logger", BOOT_PATH, "/boot/shell", "/data/sample"]
        );
        assert_eq!(image.boot_entry().path(), BOOT_PATH);
    }

    #[test]
    fn userland_image_layout_is_deterministic() {
        let entries = [
            built_entry(
                "/boot/shell",
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
                BOOT_PATH,
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

        let first = assemble_userland_image(&entries, &[]).expect("assemble image");
        let second = assemble_userland_image(&entries, &[]).expect("assemble image");
        assert_eq!(first, second);
    }

    #[test]
    fn assembled_image_exposes_program_segments_and_file_bytes() {
        let image_bytes = assemble_userland_image(
            &[built_entry(
                BOOT_PATH,
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
            )],
            &[loaded_file("/data/sample", b"hello")],
        )
        .expect("assemble image");

        let image = decode(&image_bytes).expect("assembled image must parse");
        let program = image
            .boot_entry()
            .program()
            .expect("boot record carries a program");
        let segments: Vec<_> = program.segments().collect();
        assert_eq!(segments[0].bytes, b"CODE");
        assert_eq!(segments[1].bytes, b"RW");
        assert_eq!(
            image.entry("/data/sample").expect("file record").data(),
            b"hello"
        );
    }

    #[test]
    fn star_stops_at_the_separator() {
        assert!(glob_matches("*.txt", "notes.txt"));
        assert!(!glob_matches("*.txt", "nested/notes.txt"));
        assert!(glob_matches("nested/*.txt", "nested/notes.txt"));
    }

    #[test]
    fn double_star_crosses_directories() {
        assert!(glob_matches("**", "nested/deeper/notes.txt"));
        assert!(glob_matches("**/*.txt", "nested/deeper/notes.txt"));
        // Ведущий `**/` принимает и путь без каталогов.
        assert!(glob_matches("**/*.txt", "notes.txt"));
        assert!(!glob_matches("**/*.txt", "notes.md"));
    }

    #[test]
    fn question_matches_one_character_inside_a_component() {
        assert!(glob_matches("seed-?.bin", "seed-3.bin"));
        assert!(!glob_matches("seed-?.bin", "seed-42.bin"));
        assert!(!glob_matches("seed?bin", "seed/bin"));
    }

    #[test]
    fn directory_files_are_collected_recursively_in_name_order() {
        let root = source_tree(
            "recursive",
            &[
                ("assets/b.txt", b"b"),
                ("assets/a.txt", b"a"),
                ("assets/nested/c.txt", b"c"),
            ],
        );
        let manifest = manifest_with_file("/data", "assets/**");

        let files = collect_files(&root, &manifest, &[]).expect("collect must succeed");
        assert_eq!(
            collected_paths(&files),
            ["/data/a.txt", "/data/b.txt", "/data/nested/c.txt"]
        );
        assert_eq!(files[2].bytes, b"c");
    }

    #[test]
    fn directory_glob_selects_the_files_that_enter_the_image() {
        let root = source_tree(
            "glob",
            &[
                ("assets/notes.md", b"md"),
                ("assets/sample.txt", b"txt"),
                ("assets/nested/deeper.txt", b"deeper"),
            ],
        );
        let manifest = manifest_with_file("/data", "assets/**/*.txt");

        let files = collect_files(&root, &manifest, &[]).expect("collect must succeed");
        assert_eq!(
            collected_paths(&files),
            ["/data/nested/deeper.txt", "/data/sample.txt"]
        );
    }

    #[test]
    fn a_star_selects_the_files_of_one_directory() {
        let root = source_tree(
            "flat",
            &[
                ("assets/sample.txt", b"txt"),
                ("assets/nested/deeper.txt", b"deeper"),
            ],
        );
        let manifest = manifest_with_file("/data", "assets/*");

        let files = collect_files(&root, &manifest, &[]).expect("collect must succeed");
        assert_eq!(collected_paths(&files), ["/data/sample.txt"]);
    }

    #[test]
    fn a_directory_source_without_a_pattern_is_rejected() {
        let root = source_tree("bare-directory", &[("assets/sample.txt", b"txt")]);
        let manifest = manifest_with_file("/data/assets", "assets");

        let err = collect_files(&root, &manifest, &[])
            .expect_err("directory without a pattern must fail");
        assert!(err.to_string().ends_with(
            "of path '/data/assets' is a directory; \
             its files are selected by a pattern such as 'assets/**'"
        ));
    }

    #[test]
    fn a_source_splits_at_its_first_wildcard() {
        let pattern =
            split_pattern("user/rootkeeper/assets/**/*.txt").expect("source has a pattern");
        assert_eq!(pattern.root, "user/rootkeeper/assets");
        assert_eq!(pattern.glob, "**/*.txt");

        let pattern = split_pattern("assets/*").expect("source has a pattern");
        assert_eq!(pattern.root, "assets");
        assert_eq!(pattern.glob, "*");

        assert!(split_pattern("user/rootkeeper/sample.txt").is_none());
    }

    #[test]
    fn directory_under_the_image_root_keeps_the_relative_path() {
        let root = source_tree("root-prefix", &[("assets/boot/sample", b"hi")]);
        let manifest = manifest_with_file("/", "assets/**");

        let files = collect_files(&root, &manifest, &[]).expect("collect must succeed");
        assert_eq!(collected_paths(&files), ["/boot/sample"]);
    }

    #[test]
    fn command_line_files_join_the_files_of_the_manifest() {
        let root = source_tree(
            "command-line",
            &[("assets/sample.txt", b"txt"), ("motd", b"welcome")],
        );
        let extra = [ExtraFile {
            path: "/etc/motd".to_string(),
            source: PathBuf::from("motd"),
        }];
        let manifest = manifest_with_file("/data", "assets/**");

        let files = collect_files(&root, &manifest, &extra).expect("collect must succeed");
        assert_eq!(collected_paths(&files), ["/data/sample.txt", "/etc/motd"]);
        assert_eq!(files[1].bytes, b"welcome");
    }

    #[test]
    fn one_path_filled_with_equal_content_gives_one_record() {
        let root = source_tree("dedup", &[("assets/sample.txt", b"same")]);
        let mut manifest = manifest_with_file("/data", "assets/**");
        manifest
            .files
            .push(image_file("/data/sample.txt", "assets/sample.txt"));
        let extra = [ExtraFile {
            path: "/data/sample.txt".to_string(),
            source: PathBuf::from("assets/sample.txt"),
        }];

        let files = collect_files(&root, &manifest, &extra).expect("collect must succeed");
        assert_eq!(collected_paths(&files), ["/data/sample.txt"]);
    }

    #[test]
    fn one_path_filled_with_different_content_is_rejected() {
        let root = source_tree(
            "conflict",
            &[("assets/sample.txt", b"tree"), ("other.txt", b"other")],
        );
        let mut manifest = manifest_with_file("/data", "assets/**");
        manifest
            .files
            .push(image_file("/data/sample.txt", "other.txt"));

        let err = collect_files(&root, &manifest, &[]).expect_err("conflicting content must fail");
        assert!(
            err.to_string()
                .starts_with("image path '/data/sample.txt' is filled by")
        );
        assert!(err.to_string().ends_with("with different content"));
    }

    #[test]
    fn command_line_file_parses_path_and_source() {
        let file: ExtraFile = "/etc/motd=user/rootkeeper/motd.txt"
            .parse()
            .expect("argument must parse");
        assert_eq!(file.path, "/etc/motd");
        assert_eq!(file.source, PathBuf::from("user/rootkeeper/motd.txt"));
    }

    #[test]
    fn command_line_file_without_a_source_is_rejected() {
        let err = "/etc/motd"
            .parse::<ExtraFile>()
            .expect_err("argument without a source must fail");
        assert_eq!(
            err.to_string(),
            "image file '/etc/motd' must be given as 'path=source'"
        );

        let err = "/etc/motd="
            .parse::<ExtraFile>()
            .expect_err("empty source must fail");
        assert_eq!(
            err.to_string(),
            "image file '/etc/motd=' must set both a non-empty path and a non-empty source"
        );
    }
}
