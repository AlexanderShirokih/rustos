//! Валидные образы, которыми затравливается корпус fuzz-цели `userland_image_decode`.

use anyhow::{Result, anyhow};
use component_abi::{Attribute, ExportDeclaration, encode_manifest, manifest_len};
use userland::{Program, Segment, SegmentPermissions, USERLAND_PAGE_SIZE, encode_program};
use userland_image::{BOOT_PLAN_PATH, ComponentSpec, ImageRecord, encode, encode_plan};

const BOOT_PATH: &str = "/boot/rootkeeper/exec";

const EXPORTS: &[ExportDeclaration] = &[
    ExportDeclaration::new("Hardware"),
    ExportDeclaration::filtered(
        "Display",
        &[
            &[Attribute {
                key: b"kind",
                value: b"simple-framebuffer",
            }],
            &[
                Attribute {
                    key: b"kind",
                    value: b"virtio-gpu",
                },
                Attribute {
                    key: b"role",
                    value: b"",
                },
            ],
        ],
    ),
];
const MANIFEST_LEN: usize = manifest_len(&[], EXPORTS, &[]);

/// Затравка корпуса: имя файла и байты валидного образа.
pub struct ImageSeed {
    pub name: &'static str,
    pub bytes: Vec<u8>,
}

/// Образы, проходящие оба уровня формата: таблицу записей и блоб программы.
/// Собираются кодировщиком `userland-image` - тем же, что и `userland.img`.
pub fn userland_image_seeds() -> Result<Vec<ImageSeed>> {
    Ok(vec![
        ImageSeed {
            name: "boot_program",
            bytes: image(&[ImageRecord {
                path: "/data/sample",
                data: b"hello",
            }])?,
        },
        ImageSeed {
            name: "empty_record",
            bytes: image(&[ImageRecord {
                path: "/data/empty",
                data: b"",
            }])?,
        },
        ImageSeed {
            name: "shared_window",
            bytes: image(&[
                ImageRecord {
                    path: "/data/first",
                    data: b"same",
                },
                ImageRecord {
                    path: "/data/second",
                    data: b"same",
                },
            ])?,
        },
    ])
}

fn image(files: &[ImageRecord<'_>]) -> Result<Vec<u8>> {
    let program = boot_program()?;
    let plan = encode_plan(&[ComponentSpec {
        executable: "/boot/rootkeeper",
        name: "rootkeeper",
        mountpoint: None,
        memory_limit: 0,
        restarts: 0,
    }])
    .map_err(|error| anyhow!("seed plan: {error:?}"))?;
    let mut records = vec![
        ImageRecord {
            path: BOOT_PATH,
            data: &program,
        },
        ImageRecord {
            path: BOOT_PLAN_PATH,
            data: &plan,
        },
    ];
    records.extend_from_slice(files);

    encode(&records, BOOT_PATH)
        .map_err(|err| anyhow!("Failed to serialize fuzz seed image: {err:?}"))
}

fn boot_program() -> Result<Vec<u8>> {
    let segments = [
        Segment {
            va_base: 0x4000_0000,
            mem_size: USERLAND_PAGE_SIZE,
            permissions: SegmentPermissions::ReadExecute,
            bytes: b"CODE",
        },
        Segment {
            va_base: 0x4000_1000,
            mem_size: USERLAND_PAGE_SIZE,
            permissions: SegmentPermissions::ReadWrite,
            bytes: b"RW",
        },
    ];

    encode_program(&Program {
        manifest: &encode_manifest::<MANIFEST_LEN>(&[], EXPORTS, &[]),
        entry_va: 0x4000_0000,
        stack_size: 4 * USERLAND_PAGE_SIZE,
        segments: &segments,
    })
    .map_err(|err| anyhow!("Failed to serialize fuzz seed program: {err:?}"))
}

#[cfg(test)]
mod tests {
    use userland::{ProgramView, decode_program};
    use userland_image::decode;

    use super::*;

    #[test]
    fn every_seed_decodes_down_to_program_segments() {
        for seed in userland_image_seeds().expect("seeds are encodable") {
            let image = decode(&seed.bytes).expect("seed decodes");
            let program =
                decode_program(image.boot_entry().data()).expect("boot record carries a program");
            assert_eq!(program.segments().count(), 2);

            let manifest =
                component_abi::decode_manifest(program.manifest()).expect("seed manifest");
            assert_eq!(
                manifest
                    .exports()
                    .map(|export| export.filters().count())
                    .collect::<Vec<_>>(),
                [0, 2]
            );

            userland_image::decode_plan(image.entry(BOOT_PLAN_PATH).expect("seed plan").data())
                .expect("plan decodes");
        }
    }
}
