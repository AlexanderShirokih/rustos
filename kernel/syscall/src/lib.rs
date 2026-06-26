//! Syscall-слой ядра: [`SyscallFrame`], [`SyscallOp`], [`SyscallError`],
//! [`dispatch`]. Платформенный слой реализует `SyscallFrame` для своего
//! trap-фрейма и вызывает [`dispatch`].

#![cfg_attr(not(test), no_std)]

extern crate alloc;
#[cfg(test)]
extern crate std;

mod bridge;
mod error;
mod flags;
mod irq;
mod memory;
mod numbers;
mod port;
mod process;
mod runtime;
mod thread;
mod user_io;

pub use bridge::{Origin, SyscallFrame, dispatch};
pub use numbers::SyscallOp;
pub use runtime::{SyscallRuntime, install_runtime};
pub use syscall::{
    MAX_BOOTSTRAP_HANDLES, MAX_SEGMENTS_PER_IMG, SEGMENT_ABI_VERSION, SyscallError,
    USER_IMAGE_DESC_SIZE, USER_SEGMENT_SIZE, UserImageDescAbi, UserMemFlags, UserSegmentAbi,
    decode_image_desc, decode_segment, encode_image_desc, encode_segment,
};
