use alloc::sync::Arc;

use memory::{UserVmContext, frame_allocator::FrameAllocator};
use spin::Once;

pub trait SyscallRuntime: Send + Sync {
    fn current_user_vm(&self) -> Option<UserVmContext>;

    /// Глобальный аллокатор физических фреймов для anonymous-регионов
    /// (Memory KObject Virtual). `None`, если ядро ещё не зарегистрировало
    /// его - в этом случае соответствующие syscall'ы возвращают
    /// `OutOfMemory`.
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
