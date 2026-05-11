//! ABI `userland.img` и bootstrap hello.
//!
//! `userland.img` состоит из фиксированного header, затем variable-length
//! entry records, затем payload bytes.

#![cfg_attr(not(test), no_std)]

#[cfg(test)]
extern crate std;

mod bootstrap;
mod image;

pub use bootstrap::{
    BOOTSTRAP_ABI_VERSION, BOOTSTRAP_HELLO_MAGIC, BOOTSTRAP_HELLO_SIZE, BootstrapHello,
    BootstrapHelloError, parse_bootstrap_hello,
};
pub use image::{
    USERLAND_IMAGE_ENTRY_HEADER_SIZE, USERLAND_IMAGE_ENTRY_NAME_CAPACITY,
    USERLAND_IMAGE_HEADER_SIZE, USERLAND_IMAGE_MAGIC, USERLAND_IMAGE_PAGE_SIZE,
    USERLAND_IMAGE_SEGMENT_SIZE, USERLAND_IMAGE_VERSION, UserlandImage, UserlandImageEntries,
    UserlandImageEntry, UserlandImageEntryHeader, UserlandImageError, UserlandImageHeader,
    UserlandImageSegment, UserlandImageSegments, decode_entry_header, decode_image_header,
    decode_segment,
};
