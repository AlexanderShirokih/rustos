#![no_main]

use libfuzzer_sys::fuzz_target;
use userland::{EntryView, SegmentView};

fuzz_target!(|data: &[u8]| {
    let Ok(image) = userland_image::decode(data) else {
        return;
    };

    let _ = image.entry_count();
    for entry in image.entries() {
        let _ = (entry.name(), entry.entry_va(), entry.stack_size());
        let _ = userland::validate_entry(&entry);
        for segment in entry.segments() {
            let _ = (
                segment.va_base(),
                segment.mem_size(),
                segment.permissions(),
                segment.bytes().len(),
            );
        }
    }
});
