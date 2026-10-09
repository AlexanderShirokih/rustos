#![no_main]
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::missing_asserts_for_indexing,
    clippy::cast_possible_truncation,
    clippy::unwrap_in_result,
    clippy::panic,
    clippy::std_instead_of_core
)]

use libfuzzer_sys::fuzz_target;
use userland::{ProgramView, SegmentView};

fuzz_target!(|data: &[u8]| {
    let Ok(image) = userland_image::decode(data) else {
        return;
    };

    let _ = image.entry_count();
    let _ = image.boot_entry().path();
    for entry in image.entries() {
        let _ = (entry.path(), entry.data().len(), entry.data_offset());
        // Двоичный поиск идёт по путям недоверенной таблицы и опирается на её
        // порядок, установленный декодером.
        let _ = image.entry(entry.path());

        if let Ok(plan) = userland_image::decode_plan(entry.data()) {
            for component in plan.components() {
                let _ = (
                    component.name(),
                    component.executable(),
                    component.memory_limit(),
                    component.restarts(),
                );
            }
        }

        // Запись образа несёт либо программу, либо файл-данные: разбор второй как программы
        // отказывает, и её байты дальше не идут.
        let Ok(program) = userland::decode_program(entry.data()) else {
            continue;
        };
        let _ = (
            userland_image::entry_name(entry.path()),
            program.entry_va(),
            program.stack_size(),
            program.manifest(),
        );

        if let Ok(manifest) = component_abi::decode_manifest(program.manifest()) {
            for requirement in manifest.requirements() {
                let _ = (
                    requirement.name(),
                    requirement.mode(),
                    requirement.protocol(),
                );
            }

            for export in manifest.exports() {
                let _ = export.protocol();
            }
        }

        let _ = userland::validate_program(&program);

        for segment in program.segments() {
            let _ = (
                segment.va_base(),
                segment.mem_size(),
                segment.permissions(),
                segment.bytes().len(),
            );
        }
    }
});
