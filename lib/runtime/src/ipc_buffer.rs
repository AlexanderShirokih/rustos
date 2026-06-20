//! Безопасный accessor к per-thread IPC-буферу.
//!
//! Один раз дёргает `IpcBufferAddr`-syscall, кэширует VA и отдаёт указатель
//! на [`IpcBuffer`].

use core::sync::atomic::{AtomicU64, Ordering};

use syscall::IpcBuffer;

use crate::svc::ipc_buffer_addr;

/// Кэш VA IPC-buffer'а. `0` - ещё не запрошено (валидное VA всегда > 0).
///
/// TODO(multi-thread): кэш per-process-глобальный, но IPC-buffer - per-thread.
/// Для текущего bootstrap-userspace (один поток на процесс) это корректно: VA
/// валиден для текущего потока. При появлении нескольких потоков в одном
/// процессе кэш нужно сделать per-thread (TLS) - сейчас НЕ усложняем.
static CACHED_VA: AtomicU64 = AtomicU64::new(0);

/// Возвращает VA IPC-buffer'а текущего потока, кэшируя результат первого
/// syscall'а. `None`, если у потока нет буфера (syscall вернул ошибку).
fn ipc_buffer_va() -> Option<u64> {
    let cached = CACHED_VA.load(Ordering::Relaxed);
    if cached != 0 {
        return Some(cached);
    }
    let ret = ipc_buffer_addr();
    if ret <= 0 {
        return None;
    }
    #[allow(clippy::cast_sign_loss)]
    let va = ret as u64;
    CACHED_VA.store(va, Ordering::Relaxed);
    Some(va)
}

/// Указатель на per-thread [`IpcBuffer`] текущего потока, либо `None`, если
/// у потока нет буфера.
///
/// Буфер замаплен ядром user-RW размером в одну страницу; первое обращение
/// уходит в syscall, дальше отдаётся кэшированный VA. См. замечание о
/// per-thread семантике у [`CACHED_VA`].
pub fn ipc_buffer_ptr() -> Option<*mut IpcBuffer> {
    let va = ipc_buffer_va()?;
    Some(va as *mut IpcBuffer)
}
