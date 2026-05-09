use alloc::sync::Arc;

use memory::UserVmContext;
use spin::Once;

pub trait SyscallRuntime: Send + Sync {
    fn exit_current(&self) -> !;

    fn current_user_vm(&self) -> Option<UserVmContext>;
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
