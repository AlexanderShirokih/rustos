use anyhow::{Result, anyhow, bail, ensure};
use filesystem_abi::{NAME_MAX, PATH_MAX};
use userland::{Entry, Segment, USERLAND_ENTRY_PATH_CAPACITY};
use userland_image::{COMPONENT_PROGRAM_NAME, ImageRecord, decode, encode, encode_program};

use super::{build::BuiltEntry, files::LoadedFile, program::validate_program};

const _: () = assert!(PATH_MAX == USERLAND_ENTRY_PATH_CAPACITY);

pub(crate) fn validate_image_path(path: &str) -> Result<()> {
    filesystem_abi::Path::new(path).map_err(|_| {
        anyhow!(
            "image path '{path}' must be absolute and at most {PATH_MAX} bytes, \
             with components of 1..={NAME_MAX} bytes other than '.' and '..'"
        )
    })?;
    ensure!(path.is_ascii(), "image path '{path}' must be ASCII");
    Ok(())
}

pub(super) fn component_record_path(directory: &str) -> String {
    format!("{directory}/{COMPONENT_PROGRAM_NAME}")
}

fn under_directory(path: &str, directory: &str) -> bool {
    path.strip_prefix(directory)
        .is_some_and(|rest| rest.starts_with('/') && rest.len() > 1)
}

pub(super) fn assemble_userland_image(
    entries: &[BuiltEntry],
    files: &[LoadedFile],
) -> Result<Vec<u8>> {
    let boot_path = validate_built_entries(entries, files)?;

    let blobs = entries
        .iter()
        .map(build_program_blob)
        .collect::<Result<Vec<_>>>()?;
    let records: Vec<ImageRecord<'_>> = entries
        .iter()
        .zip(&blobs)
        .map(|(entry, blob)| ImageRecord {
            path: &entry.record_path,
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

fn validate_component_directories(entries: &[BuiltEntry], files: &[LoadedFile]) -> Result<()> {
    // Каталог компонента - его файловая область: чужих записей в ней нет, и
    // назвать что-то вне её компонент не может.
    for (index, entry) in entries.iter().enumerate() {
        let directory = entry.program.path.as_str();
        for earlier in entries.iter().take(index) {
            let other = earlier.program.path.as_str();
            ensure!(
                !under_directory(directory, other) && !under_directory(other, directory),
                "component directory '{directory}' lies inside '{other}'"
            );
        }
    }
    for file in files {
        let owner = entries
            .iter()
            .find(|entry| under_directory(&file.path, &entry.program.path));
        ensure!(
            owner.is_some(),
            "image record '{}' lies outside every component directory",
            file.path
        );
    }
    Ok(())
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

    validate_component_directories(entries, files)?;
    let paths = entries
        .iter()
        .map(|entry| entry.record_path.as_str())
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
    match boot_entries.as_slice() {
        [boot] => {
            ensure!(
                boot.segments
                    .iter()
                    .any(|segment| !segment.bytes.is_empty()),
                "boot entry '{}' payload must not be empty",
                boot.program.path
            );
            Ok(&boot.record_path)
        }
        [] => bail!("userland image must contain exactly one boot entry; found none"),
        found => bail!(
            "userland image must contain exactly one boot entry; found {}",
            found.len()
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use userland::{EntryView, SegmentPermissions};
    use userland_image::{BOOT_STAGE_ONE_DIRECTORY, decode};

    use super::{
        super::{build::BuiltSegment, program::ResolvedProgram},
        *,
    };
    use crate::manifest::ProgramManifest;

    const BOOT_PATH: &str = "/boot/rootkeeper";

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
            record_path: component_record_path(path),
            program: resolved_program("package", path, boot, 0x4000),
            entry_va,
            segments: segments.to_vec(),
            manifest: ProgramManifest::default(),
        }
    }

    fn loaded_file(path: &str, bytes: &[u8]) -> LoadedFile {
        LoadedFile {
            path: path.to_string(),
            source: PathBuf::from("source"),
            bytes: bytes.to_vec(),
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
        let err =
            assemble_userland_image(&entries, &[loaded_file("/boot/rootkeeper/./sample", b"hi")])
                .expect_err("path outside the contract must fail");
        assert!(
            err.to_string()
                .starts_with("image path '/boot/rootkeeper/./sample'")
        );
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
        let err =
            assemble_userland_image(&entries, &[loaded_file("/boot/rootkeeper/motdé", b"hi")])
                .expect_err("non-ascii path must fail");
        assert_eq!(
            err.to_string(),
            "image path '/boot/rootkeeper/motdé' must be ASCII"
        );
    }

    #[test]
    fn a_qualified_instance_name_holds_as_a_stage_one_record_path() {
        validate_image_path(&format!("{BOOT_STAGE_ONE_DIRECTORY}/foo::helper"))
            .expect("a qualified instance name holds as an image record path");
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
        let err = assemble_userland_image(
            &entries,
            &[loaded_file(&component_record_path(BOOT_PATH), b"clash")],
        )
        .expect_err("duplicate path must fail");
        assert_eq!(
            err.to_string(),
            format!(
                "image path '{}' is used by more than one record",
                component_record_path(BOOT_PATH)
            )
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

        let image_bytes =
            assemble_userland_image(&entries, &[loaded_file("/boot/rootkeeper/sample", b"hi")])
                .expect("assemble image");
        let image = decode(&image_bytes).expect("assembled image must parse");

        let paths: Vec<_> = image.entries().map(|entry| entry.path()).collect();
        assert_eq!(
            paths,
            [
                "/boot/logger/program",
                "/boot/rootkeeper/program",
                "/boot/rootkeeper/sample",
                "/boot/shell/program",
            ]
        );
        assert_eq!(image.boot_entry().path(), component_record_path(BOOT_PATH));
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
            &[loaded_file("/boot/rootkeeper/sample", b"hello")],
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
            image
                .entry("/boot/rootkeeper/sample")
                .expect("file record")
                .data(),
            b"hello"
        );
    }
}
