use std::{
    ffi::OsStr,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use object::{Architecture, BinaryFormat, Object, ObjectKind, ObjectSegment, SegmentFlags, elf};
use serde::Deserialize;
use userland_abi::{
    ImageEntryInput, ImageSegmentInput, USERLAND_IMAGE_ENTRY_NAME_CAPACITY,
    USERLAND_IMAGE_PAGE_SIZE, USERLAND_IMAGE_VERSION, UserlandImage, build_userland_image,
};

const USERLAND_MANIFEST_VERSION: u16 = USERLAND_IMAGE_VERSION;
const USERLAND_STACK_ALIGN: u64 = USERLAND_IMAGE_PAGE_SIZE;

#[derive(Debug, Deserialize)]
struct UserlandManifest {
    #[serde(default = "default_manifest_version")]
    version: u16,
    entries: Vec<UserlandManifestEntry>,
}

#[derive(Clone, Debug, Deserialize)]
struct UserlandManifestEntry {
    name: String,
    package: String,
    bin: String,
    #[serde(default)]
    bootstrap: bool,
    stack_size: u64,
}

#[derive(Clone, Debug)]
struct BuiltEntry {
    manifest_order: usize,
    manifest: UserlandManifestEntry,
    entry_va: u64,
    segments: Vec<BuiltSegment>,
}

#[derive(Clone, Debug)]
struct BuiltSegment {
    bytes: Vec<u8>,
    va_base: u64,
    mem_size: u64,
    flags: u32,
}

fn default_manifest_version() -> u16 {
    USERLAND_MANIFEST_VERSION
}

pub fn build_userland(project_root: &Path, build_dir: &Path) -> Result<PathBuf> {
    let manifest_path = project_root.join("userland/manifest.toml");
    let manifest = load_manifest(&manifest_path)?;
    let built_entries = manifest
        .entries
        .iter()
        .enumerate()
        .map(|(manifest_order, entry)| build_manifest_entry(project_root, manifest_order, entry))
        .collect::<Result<Vec<_>>>()?;

    let blob = assemble_userland_image(&built_entries)?;
    let output_path = build_dir.join("userland.img");
    fs::write(&output_path, blob)
        .with_context(|| format!("Failed to write userland image '{}'", output_path.display()))?;
    Ok(output_path)
}

fn load_manifest(path: &Path) -> Result<UserlandManifest> {
    let manifest_src = fs::read_to_string(path)
        .with_context(|| format!("Failed to read userland manifest '{}'", path.display()))?;
    parse_manifest(&manifest_src)
}

fn parse_manifest(src: &str) -> Result<UserlandManifest> {
    let manifest: UserlandManifest =
        toml::from_str(src).context("Failed to parse userland manifest TOML")?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

fn validate_manifest(manifest: &UserlandManifest) -> Result<()> {
    ensure!(
        manifest.version == USERLAND_MANIFEST_VERSION,
        "Unsupported userland manifest version {}; expected {}",
        manifest.version,
        USERLAND_MANIFEST_VERSION
    );
    ensure!(
        !manifest.entries.is_empty(),
        "userland manifest must contain at least one [[entries]] item"
    );

    match manifest
        .entries
        .iter()
        .filter(|entry| entry.bootstrap)
        .count()
    {
        1 => {}
        0 => bail!("userland manifest must contain exactly one bootstrap entry; found none"),
        count => bail!("userland manifest must contain exactly one bootstrap entry; found {count}"),
    }

    for entry in &manifest.entries {
        validate_manifest_entry(entry)?;
    }

    Ok(())
}

fn validate_manifest_entry(entry: &UserlandManifestEntry) -> Result<()> {
    ensure!(
        !entry.name.is_empty() && entry.name.len() <= USERLAND_IMAGE_ENTRY_NAME_CAPACITY,
        "userland entry '{}' name must be 1..={} bytes",
        entry.name,
        USERLAND_IMAGE_ENTRY_NAME_CAPACITY
    );
    ensure!(
        entry.name.is_ascii(),
        "userland entry '{}' name must be ASCII",
        entry.name
    );
    ensure!(
        !entry.package.is_empty(),
        "userland entry '{}' package must not be empty",
        entry.name
    );
    ensure!(
        !entry.bin.is_empty(),
        "userland entry '{}' bin must not be empty",
        entry.name
    );
    ensure!(
        entry.stack_size != 0 && entry.stack_size.is_multiple_of(USERLAND_STACK_ALIGN),
        "userland entry '{}' stack_size must be a non-zero multiple of {} bytes; got {}",
        entry.name,
        USERLAND_STACK_ALIGN,
        entry.stack_size
    );
    Ok(())
}

fn build_manifest_entry(
    project_root: &Path,
    manifest_order: usize,
    manifest: &UserlandManifestEntry,
) -> Result<BuiltEntry> {
    run_userland_build(project_root, manifest)?;
    let binary_path = userland_binary_path(project_root, &manifest.bin);
    read_and_extract_entry(manifest_order, &binary_path, manifest)
}

fn run_userland_build(project_root: &Path, manifest: &UserlandManifestEntry) -> Result<()> {
    let status = Command::new("cargo")
        .args([
            "build",
            "--package",
            &manifest.package,
            "--bin",
            &manifest.bin,
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
            manifest.name,
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
    manifest_order: usize,
    path: &Path,
    manifest: &UserlandManifestEntry,
) -> Result<BuiltEntry> {
    let mut payload = Vec::new();
    File::open(path)
        .with_context(|| format!("Failed to open userland ELF '{}'", path.display()))?
        .read_to_end(&mut payload)
        .with_context(|| format!("Failed to read userland ELF '{}'", path.display()))?;

    ensure!(
        path.extension() != Some(OsStr::new("d")),
        "unexpected dep-info path for userland entry '{}'",
        manifest.name
    );

    let file = object::File::parse(payload.as_slice()).with_context(|| {
        format!(
            "Failed to parse payload ELF for userland entry '{}'",
            manifest.name
        )
    })?;

    validate_file(&file, manifest)?;
    let entry_va = file.entry();
    let segments = extract_segments(&file, manifest)?;
    ensure!(
        segments.iter().any(|segment| {
            segment.flags == 2
                && entry_va >= segment.va_base
                && entry_va < segment.va_base.saturating_add(segment.mem_size)
        }),
        "userland entry '{}' entry point is outside executable PT_LOAD",
        manifest.name
    );

    Ok(BuiltEntry {
        manifest_order,
        manifest: manifest.clone(),
        entry_va,
        segments,
    })
}

fn validate_file(file: &object::File<'_>, manifest: &UserlandManifestEntry) -> Result<()> {
    ensure!(
        file.format() == BinaryFormat::Elf,
        "userland entry '{}' payload is not ELF",
        manifest.name
    );
    ensure!(
        file.architecture() == Architecture::Aarch64,
        "userland entry '{}' payload must target AArch64",
        manifest.name
    );
    ensure!(
        file.kind() == ObjectKind::Executable,
        "userland entry '{}' payload must be ET_EXEC",
        manifest.name
    );
    ensure!(
        file.is_little_endian(),
        "userland entry '{}' payload must be little-endian",
        manifest.name
    );
    ensure!(
        file.is_64(),
        "userland entry '{}' payload must be ELF64",
        manifest.name
    );
    Ok(())
}

fn extract_segments(
    file: &object::File<'_>,
    manifest: &UserlandManifestEntry,
) -> Result<Vec<BuiltSegment>> {
    let mut segments = Vec::new();

    for segment in file.segments() {
        let bytes = segment
            .data()
            .with_context(|| format!("Failed to read PT_LOAD bytes for '{}'", manifest.name))?;
        let flags = map_segment_flags(segment.flags(), manifest)?;
        segments.push(BuiltSegment {
            bytes: bytes.to_vec(),
            va_base: segment.address(),
            mem_size: segment.size(),
            flags,
        });
    }

    ensure!(
        !segments.is_empty(),
        "userland entry '{}' must contain at least one PT_LOAD segment",
        manifest.name
    );

    Ok(segments)
}

fn map_segment_flags(flags: SegmentFlags, manifest: &UserlandManifestEntry) -> Result<u32> {
    let SegmentFlags::Elf { p_flags } = flags else {
        bail!(
            "userland entry '{}' must expose ELF PT_LOAD segment flags",
            manifest.name
        );
    };

    let readable = (p_flags & elf::PF_R) != 0;
    let writable = (p_flags & elf::PF_W) != 0;
    let executable = (p_flags & elf::PF_X) != 0;

    match (readable, writable, executable) {
        (true, true, false) => Ok(0),
        (true, false, false) => Ok(1),
        (true, false, true) => Ok(2),
        _ => bail!(
            "userland entry '{}' has unsupported PT_LOAD flags {p_flags:#x}",
            manifest.name
        ),
    }
}

fn assemble_userland_image(entries: &[BuiltEntry]) -> Result<Vec<u8>> {
    validate_built_entries(entries)?;

    let ordered = order_entries(entries);

    // Сериализацию делает writer userland-abi; здесь - только сборка входа.
    let segments: Vec<Vec<ImageSegmentInput<'_>>> = ordered
        .iter()
        .map(|entry| {
            entry
                .segments
                .iter()
                .map(|segment| ImageSegmentInput {
                    va_base: segment.va_base,
                    mem_size: segment.mem_size,
                    flags: segment.flags,
                    bytes: &segment.bytes,
                })
                .collect()
        })
        .collect();
    let inputs: Vec<ImageEntryInput<'_>> = ordered
        .iter()
        .zip(&segments)
        .map(|(entry, segments)| ImageEntryInput {
            name: &entry.manifest.name,
            entry_va: entry.entry_va,
            stack_size: entry.manifest.stack_size,
            segments,
        })
        .collect();

    let image = build_userland_image(&inputs)
        .map_err(|err| anyhow!("failed to serialize userland image: {err:?}"))?;

    UserlandImage::parse(&image)
        .map_err(|err| anyhow!("assembled userland image is invalid: {err:?}"))?;
    Ok(image)
}

fn validate_built_entries(entries: &[BuiltEntry]) -> Result<()> {
    ensure!(
        !entries.is_empty(),
        "userland image must contain at least one entry"
    );

    for entry in entries {
        validate_manifest_entry(&entry.manifest)?;
        ensure!(
            !entry.segments.is_empty(),
            "userland entry '{}' must contain at least one segment",
            entry.manifest.name
        );
    }

    let bootstrap_entries = entries
        .iter()
        .filter(|entry| entry.manifest.bootstrap)
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
                bootstrap.manifest.name
            );
        }
        0 => bail!("userland image must contain exactly one bootstrap entry; found none"),
        count => bail!("userland image must contain exactly one bootstrap entry; found {count}"),
    }

    Ok(())
}

fn order_entries(entries: &[BuiltEntry]) -> Vec<&BuiltEntry> {
    let mut ordered = entries.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|entry| (!entry.manifest.bootstrap, entry.manifest_order));
    ordered
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_src(entries: &[(&str, bool, u64)]) -> String {
        let mut src = String::from("version = 1\n");
        for (name, bootstrap, stack_size) in entries {
            src.push_str("\n[[entries]]\n");
            src.push_str(&format!("name = \"{name}\"\n"));
            src.push_str(&format!("package = \"{name}\"\n"));
            src.push_str(&format!("bin = \"{name}\"\n"));
            if *bootstrap {
                src.push_str("bootstrap = true\n");
            }
            src.push_str(&format!("stack_size = {stack_size}\n"));
        }
        src
    }

    fn built_segment(bytes: &[u8], va_base: u64, mem_size: u64, flags: u32) -> BuiltSegment {
        BuiltSegment {
            bytes: bytes.to_vec(),
            va_base,
            mem_size,
            flags,
        }
    }

    fn manifest_entry(
        manifest_order: usize,
        name: &str,
        bootstrap: bool,
        entry_va: u64,
        segments: &[BuiltSegment],
    ) -> BuiltEntry {
        BuiltEntry {
            manifest_order,
            manifest: UserlandManifestEntry {
                name: name.to_string(),
                package: name.to_string(),
                bin: name.to_string(),
                bootstrap,
                stack_size: 0x4000,
            },
            entry_va,
            segments: segments.to_vec(),
        }
    }

    #[test]
    fn manifest_rejects_zero_bootstrap_entries() {
        let err = parse_manifest(&manifest_src(&[
            ("rootkeeper", false, 0x4000),
            ("shell", false, 0x4000),
        ]))
        .expect_err("manifest without bootstrap must fail");
        assert_eq!(
            err.to_string(),
            "userland manifest must contain exactly one bootstrap entry; found none"
        );
    }

    #[test]
    fn manifest_rejects_multiple_bootstrap_entries() {
        let err = parse_manifest(&manifest_src(&[
            ("rootkeeper", true, 0x4000),
            ("shell", true, 0x4000),
        ]))
        .expect_err("manifest with multiple bootstrap entries must fail");
        assert_eq!(
            err.to_string(),
            "userland manifest must contain exactly one bootstrap entry; found 2"
        );
    }

    #[test]
    fn validate_manifest_entry_rejects_bad_stack_size() {
        let err =
            parse_manifest(&manifest_src(&[("rootkeeper", true, 123)])).expect_err("bad stack");
        assert!(
            err.to_string()
                .contains("stack_size must be a non-zero multiple of 4096 bytes; got 123")
        );
    }

    #[test]
    fn bootstrap_entry_must_not_be_empty() {
        let entries = [manifest_entry(
            0,
            "rootkeeper",
            true,
            0x4000_0000,
            &[built_segment(&[], 0x4000_0000, 0x1000, 2)],
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
            manifest_entry(
                2,
                "shell",
                false,
                0x5000_0000,
                &[built_segment(b"ELF", 0x5000_0000, 0x1000, 2)],
            ),
            manifest_entry(
                0,
                "rootkeeper",
                true,
                0x4000_0000,
                &[built_segment(b"BOOT", 0x4000_0000, 0x1000, 2)],
            ),
            manifest_entry(
                1,
                "logger",
                false,
                0x4800_0000,
                &[built_segment(b"LOG", 0x4800_0000, 0x1000, 2)],
            ),
        ];

        let image_bytes = assemble_userland_image(&entries).expect("assemble image");
        let image = UserlandImage::parse(&image_bytes).expect("assembled image must parse");
        assert_eq!(image.bootstrap_entry().name_bytes(), b"rootkeeper");
        assert_eq!(image.entry(1).expect("entry 1").name_bytes(), b"logger");
        assert_eq!(image.entry(2).expect("entry 2").name_bytes(), b"shell");
    }

    #[test]
    fn userland_image_layout_is_deterministic() {
        let entries = [
            manifest_entry(
                1,
                "shell",
                false,
                0x5000_0000,
                &[built_segment(b"ELF", 0x5000_0000, 0x1000, 2)],
            ),
            manifest_entry(
                0,
                "rootkeeper",
                true,
                0x4000_0000,
                &[
                    built_segment(b"CODE", 0x4000_0000, 0x1000, 2),
                    built_segment(b"RW", 0x4000_1000, 0x1000, 0),
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
            manifest_entry(
                0,
                "rootkeeper",
                true,
                0x4000_0000,
                &[
                    built_segment(b"CODE", 0x4000_0000, 0x1000, 2),
                    built_segment(b"RW", 0x4000_1000, 0x1000, 0),
                ],
            ),
            manifest_entry(
                1,
                "shell",
                false,
                0x5000_0000,
                &[built_segment(b"ELF", 0x5000_0000, 0x1000, 2)],
            ),
        ])
        .expect("assemble image");

        let image = UserlandImage::parse(&image_bytes).expect("assembled image must parse");
        let bootstrap = image.bootstrap_entry();
        let header = bootstrap.header();
        let payload_offset = usize::try_from(header.payload_offset).expect("payload offset fits");
        let payload_size = usize::try_from(header.payload_size).expect("payload size fits");
        assert_eq!(
            bootstrap.payload(),
            &image_bytes[payload_offset..payload_offset + payload_size]
        );

        let first_segment = bootstrap.segment(0).expect("segment 0");
        let first_offset = usize::try_from(first_segment.file_offset).expect("offset fits");
        let first_size = usize::try_from(first_segment.file_size).expect("size fits");
        assert_eq!(
            &image_bytes[first_offset..first_offset + first_size],
            b"CODE"
        );
        assert!(first_offset >= payload_offset);
        assert!(first_offset + first_size <= payload_offset + payload_size);

        let second_segment = bootstrap.segment(1).expect("segment 1");
        let second_offset = usize::try_from(second_segment.file_offset).expect("offset fits");
        let second_size = usize::try_from(second_segment.file_size).expect("size fits");
        assert_eq!(
            &image_bytes[second_offset..second_offset + second_size],
            b"RW"
        );
        assert!(second_offset >= payload_offset);
        assert!(second_offset + second_size <= payload_offset + payload_size);
    }
}
