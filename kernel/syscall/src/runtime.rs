use alloc::sync::Arc;

use memory::{UserVmContext, frame_allocator::FrameAllocator};
use spin::Once;

pub trait SyscallRuntime: Send + Sync {
    fn current_user_vm(&self) -> Option<UserVmContext>;

    /// User-VA per-thread IPC-буфера текущего потока, либо `None`, если у
    /// потока нет буфера (kernel-поток).
    fn current_ipc_buffer_va(&self) -> Option<u64>;

    /// Аллокатор фреймов для anonymous Memory KObject. `None` до регистрации
    /// - соответствующие syscall'ы вернут `OutOfMemory`.
    fn frame_allocator(&self) -> Option<&'static (dyn FrameAllocator + Send + Sync)>;
}

static RUNTIME: Once<Arc<dyn SyscallRuntime>> = Once::new();

pub fn install_runtime(runtime: Arc<dyn SyscallRuntime>) {
    assert!(
        RUNTIME.get().is_none(),
        "SyscallRuntime is already installed"
    );
    let _ = RUNTIME.call_once(|| runtime);
}

pub fn runtime() -> &'static Arc<dyn SyscallRuntime> {
    RUNTIME
        .get()
        .expect("SyscallRuntime must be installed before syscall dispatch")
}
