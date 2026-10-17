//! Безопасный accessor к per-thread IPC-буферу.
//!
//! IPC-буфер выдаётся ядром per-thread, его VA лежит в `TPIDRRO_EL0` потока. Вне RustOS на
//! aarch64 `ipc_buffer_ptr` отдаёт `None`.

#[cfg(all(target_os = "molecular", target_arch = "aarch64"))]
use core::arch::asm;

use syscall::IpcBuffer;

#[cfg(all(target_os = "molecular", target_arch = "aarch64"))]
fn ipc_buffer_va() -> u64 {
    let va: u64;
    // SAFETY: `TPIDRRO_EL0` читается на EL0 без побочных эффектов, его пишет только ядро.
    unsafe {
        asm!(
            "mrs {va}, tpidrro_el0",
            va = out(reg) va,
            options(nomem, nostack, preserves_flags),
        );
    }
    va
}

#[cfg(not(all(target_os = "molecular", target_arch = "aarch64")))]
fn ipc_buffer_va() -> u64 {
    0
}

/// Указатель на per-thread [`IpcBuffer`] текущего потока, либо `None`, если
/// у потока нет буфера.
///
/// Буфер замаплен ядром user-RW размером в одну страницу. Указатель эксклюзивен для
/// текущего потока и действителен до его завершения, но за пределы этого потока его
/// брать нельзя.
pub fn ipc_buffer_ptr() -> Option<*mut IpcBuffer> {
    match ipc_buffer_va() {
        0 => None,
        va => Some(va as *mut IpcBuffer),
    }
}
