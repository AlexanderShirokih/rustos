//! Читает из trap-фрейма номер операции и аргументы, исполняет операцию ядра и кладёт
//! результат обратно во фрейм.
//!
//! Платформенный слой реализует [`SyscallFrame`] для своего trap-фрейма и передаёт фрейм в
//! [`dispatch`]. Номер операции назван [`SyscallOp`], отказ - [`SyscallError`]. Оба типа
//! объявлены ABI-крейтом `syscall` и реэкспортированы отсюда.
//!
//! [`install_runtime`] связывает слой с планировщиком реализацией [`SyscallRuntime`]. Подъём
//! ядра вызывает её один раз до первого системного вызова.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(test, allow(clippy::unimplemented, clippy::unwrap_in_result))]

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

const _: () = assert!(
    ::memory::PAGE_SIZE.get() == ::syscall::PAGE_SIZE,
    "syscall ABI page size disagrees with the kernel translation granule"
);

pub use bridge::{Origin, SyscallFrame, dispatch};
pub use numbers::SyscallOp;
pub use runtime::{CurrentIpcContext, SyscallRuntime, install_runtime};
pub use syscall::{
    MAX_SEGMENTS_PER_IMG, SEGMENT_ABI_VERSION, SyscallError, USER_IMAGE_DESC_SIZE,
    USER_SEGMENT_SIZE, UserImageDescAbi, UserMemFlags, UserSegmentAbi, decode_image_desc,
    decode_segment, encode_image_desc, encode_segment,
};
