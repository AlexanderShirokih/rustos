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
mod memory;
mod numbers;
mod port;
mod process;
mod runtime;
mod spawn_abi;
mod thread;
mod user_io;

pub use bridge::{Origin, SyscallFrame, dispatch};
pub use numbers::SyscallOp;
pub use runtime::{SyscallRuntime, install_runtime};
pub use spawn_abi::{
    MAX_BOOTSTRAP_HANDLES, MAX_SEGMENTS_PER_IMG, SEGMENT_ABI_VERSION, USER_IMAGE_DESC_SIZE,
    USER_SEGMENT_SIZE, UserImageDescAbi, UserSegmentAbi, decode_image_desc, decode_segment,
};
pub use syscall::{SyscallError, UserMemFlags};
