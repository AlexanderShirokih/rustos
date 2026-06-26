//! Номера операций syscall-ABI.

#![cfg_attr(not(test), no_std)]
#![feature(const_cmp, const_trait_impl)]

mod error;
mod flags;
mod handle;
mod ipc_buffer;
mod memory;
mod rights;
mod signal;
mod syscallop;
mod timeout;

pub use error::{SyscallError, SYSCALL_RETURN_TIMEOUT};
pub use flags::{InvalidUserMemFlags, UserMemFlags};
pub use handle::{Handle, RawHandle, WaitItem};
pub use ipc_buffer::{
    DecodedTag, IPC_BUFFER_DATA_MAX, IPC_BUFFER_MAX_CAPS, IpcBuffer, decode_tag, encode_tag,
};
pub use memory::MemoryAccess;
pub use rights::Rights;
pub use signal::{WakeCount, SIGNALED};
pub use syscallop::SyscallOp;
pub use timeout::{PORT_TIMEOUT_INFINITE, PORT_TIMEOUT_POLL, Timeout};
