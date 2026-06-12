//! ABI `userland.img` и bootstrap-кадры (hello, log).
//!
//! `userland.img` состоит из фиксированного header, затем variable-length
//! entry records, затем payload bytes.

#![cfg_attr(not(test), no_std)]

#[cfg(any(test, feature = "alloc"))]
extern crate alloc;
#[cfg(test)]
extern crate std;

mod bootstrap;
#[cfg(any(test, feature = "alloc"))]
mod builder;
mod image;
mod syscall;

pub use bootstrap::{
    BOOTSTRAP_ABI_VERSION, BOOTSTRAP_HELLO_MAGIC, BOOTSTRAP_HELLO_SIZE, BOOTSTRAP_LOG_FRAME_MAX,
    BOOTSTRAP_LOG_HEADER_SIZE, BOOTSTRAP_LOG_MAGIC, BOOTSTRAP_LOG_PAYLOAD_MAX, BootstrapHello,
    BootstrapHelloError, BootstrapLogError, encode_bootstrap_log, parse_bootstrap_hello,
    parse_bootstrap_log,
};
#[cfg(any(test, feature = "alloc"))]
pub use builder::{
    ImageBuildError, ImageEntryInput, ImageSegmentInput, align_file_offset, build_userland_image,
};
pub use image::{
    SegmentPermissions, USERLAND_IMAGE_ENTRY_HEADER_SIZE, USERLAND_IMAGE_ENTRY_NAME_CAPACITY,
    USERLAND_IMAGE_HEADER_SIZE, USERLAND_IMAGE_MAGIC, USERLAND_IMAGE_PAGE_SIZE,
    USERLAND_IMAGE_SEGMENT_SIZE, USERLAND_IMAGE_VERSION, UserlandImage, UserlandImageEntries,
    UserlandImageEntry, UserlandImageEntryHeader, UserlandImageError, UserlandImageHeader,
    UserlandImageSegment, UserlandImageSegments, decode_entry_header, decode_image_header,
    decode_segment,
};
pub use syscall::{
    CHANNEL_SIGNAL_PEER_CLOSED, MAILBOX_PACKET_SIZE, PROCESS_SIGNAL_TERMINATED,
    SYSCALL_RETURN_SHOULD_WAIT, SyscallOp,
};
