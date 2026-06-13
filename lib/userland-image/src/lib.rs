//! Адаптер бинарного формата `userland.img` без копирования данных.

#![cfg_attr(not(test), no_std)]

#[cfg(any(test, feature = "alloc"))]
extern crate alloc;

#[cfg(any(test, feature = "alloc"))]
mod builder;
mod image;

#[cfg(any(test, feature = "alloc"))]
pub use builder::{ImageEncodeError, encode};
pub use image::{
    DecodedEntries, DecodedEntry, DecodedImage, ImageDecodeError, USERLAND_IMAGE_MAGIC,
    USERLAND_IMAGE_VERSION, decode,
};

#[cfg(test)]
mod public_api_tests {
    use userland::{Entry, EntryView, Segment, SegmentPermissions};

    use super::{decode, encode};

    #[test]
    fn encoder_rejects_empty_images() {
        let entries: [Entry<'_>; 0] = [];
        assert_eq!(encode(&entries), Err(super::ImageEncodeError::NoEntries));
    }

    #[test]
    fn decoded_entries_expose_semantic_views() {
        let segments = [Segment {
            va_base: 0x4000_0000,
            mem_size: 0x1000,
            permissions: SegmentPermissions::ReadExecute,
            bytes: b"CODE",
        }];
        let entries = [Entry {
            name: "rootkeeper",
            entry_va: 0x4000_0000,
            stack_size: 0x4000,
            segments: &segments,
        }];

        let bytes = encode(&entries).expect("encode");
        let image = decode(&bytes).expect("decode");
        let entry = image.bootstrap_entry();

        assert_eq!(entry.name(), "rootkeeper");
        assert_eq!(entry.entry_va(), 0x4000_0000);
        assert_eq!(entry.segments().next().expect("segment").bytes, b"CODE");
    }
}
