//! Безопасный accessor к per-thread IPC-буферу.
//!
//! IPC-буфер выдаётся ядром per-thread. VA запрашивается через syscall на
//! каждый вызов (кэш недопустим: разные потоки имеют разные VA).

use molecular_rt::ipc_buffer_addr;
use syscall::IpcBuffer;

fn ipc_buffer_va() -> Option<u64> {
    let ret = ipc_buffer_addr();
    if ret <= 0 {
        return None;
    }
    #[allow(clippy::cast_sign_loss)]
    Some(ret as u64)
}

/// Указатель на per-thread [`IpcBuffer`] текущего потока, либо `None`, если
/// у потока нет буфера.
///
/// Буфер замаплен ядром user-RW размером в одну страницу.
pub fn ipc_buffer_ptr() -> Option<*mut IpcBuffer> {
    let va = ipc_buffer_va()?;
    Some(va as *mut IpcBuffer)
}
