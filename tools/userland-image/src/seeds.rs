//! Валидные образы, которыми затравливается корпус fuzz-цели `userland_image_decode`.

use anyhow::{Result, anyhow};
use userland::{Entry, Segment, SegmentPermissions, USERLAND_PAGE_SIZE};
use userland_image::{ImageRecord, encode, encode_program};

const BOOT_PATH: &str = "/boot/rootkeeper";

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
    ])
}

fn image(files: &[ImageRecord<'_>]) -> Result<Vec<u8>> {
    let program = boot_program()?;
    let mut records = vec![ImageRecord {
        path: BOOT_PATH,
        data: &program,
    }];
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

    encode_program(&Entry {
        path: BOOT_PATH,
        entry_va: 0x4000_0000,
        stack_size: 4 * USERLAND_PAGE_SIZE,
        segments: &segments,
    })
    .map_err(|err| anyhow!("Failed to serialize fuzz seed program: {err:?}"))
}

#[cfg(test)]
mod tests {
    use userland::EntryView;
    use userland_image::decode;

    use super::*;

    #[test]
    fn every_seed_decodes_down_to_program_segments() {
        for seed in userland_image_seeds().expect("seeds are encodable") {
            let image = decode(&seed.bytes).expect("seed decodes");
            let program = image
                .boot_entry()
                .program()
                .expect("boot record carries a program");
            assert_eq!(program.segments().count(), 2);
        }
    }
}
