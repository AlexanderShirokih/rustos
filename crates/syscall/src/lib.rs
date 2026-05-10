//! Syscall-слой ядра.
//!
//! Архитектурно-независимая часть syscall-механизма: трейт
//! [`SyscallFrame`], номера операций [`SyscallOp`], ошибки
//! [`SyscallError`] и универсальный диспатчер [`dispatch`]. Платформенный
//! слой реализует `SyscallFrame` для своего trap-фрейма и вызывает
//! [`dispatch`].
//!
//! ABI:
//! - номер операции - 16-битное целое, передаваемое платформой через
//!   [`SyscallFrame::op_raw`];
//! - аргументы - до шести значений `u64`, доступных через
//!   [`SyscallFrame::arg`];
//! - возврат - `i64`: успех `[0, i64::MAX]`, ошибка
//!   `-(SyscallError as u32 as i64)` в `[-MAX_ERR .. -1]`;
//! - `thread_exit` не возвращается;
//! - syscall, вызванный из kernel-контекста, отвергается с
//!   [`SyscallError::KernelOriginated`] - kernel-side IPC должен ходить
//!   напрямую через `kobject`.

#![cfg_attr(not(test), no_std)]

extern crate alloc;
#[cfg(test)]
extern crate std;

mod bridge;
mod channel;
mod error;
mod flags;
mod memory;
mod numbers;
mod runtime;

pub use bridge::{Origin, SyscallFrame, dispatch};
pub use error::SyscallError;
pub use flags::UserMemFlags;
pub use numbers::SyscallOp;
pub use runtime::{SyscallRuntime, install_runtime};
