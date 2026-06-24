use alloc::sync::Arc;

use kobject::{
    IpcError, LoadImageError, ProcessObject, SpawnError, StartProcessError, ThreadObject,
    UserImageInstall, UserStartSpec, UserThreadEntry,
};
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

    /// `Arc<ThreadObject>` текущего потока. `None`, если планировщик ещё
    /// не инициализирован и текущий поток не определён.
    fn current_thread_object(&self) -> Option<Arc<ThreadObject>>;

    /// `Arc<ProcessObject>` процесса, к которому привязан текущий поток.
    /// `None`, если планировщик ещё не инициализирован и текущий процесс не
    /// определён.
    fn current_process_object(&self) -> Option<Arc<ProcessObject>>;

    /// Создаёт user-процесс с пустым адресным пространством и handle-таблицей.
    fn create_empty_process(&self, name: &str) -> Result<Arc<ProcessObject>, SpawnError>;

    /// Создаёт user-поток в указанном процессе и помещает его в ready-queue.
    fn create_user_thread(
        &self,
        process: &Arc<ProcessObject>,
        entry: UserThreadEntry,
    ) -> Result<Arc<ThreadObject>, SpawnError>;

    /// Завершает поток. Поднимает terminated-флаг потока. На нуле - поднимает
    /// terminated-флаг владеющего процесса.
    fn terminate_thread(&self, thread: &Arc<ThreadObject>, exit_code: i32) -> Result<(), IpcError>;

    /// Завершает все потоки процесса. Для каждого потока поднимает
    /// terminated-флаг, при достижении нуля - terminated-флаг процесса.
    fn terminate_process(
        &self,
        process: &Arc<ProcessObject>,
        exit_code: i32,
    ) -> Result<(), IpcError>;

    /// Устанавливает регионы образа в child AS, маппит user-стек и
    /// прикрепляет per-process `UserVmAllocator`.
    fn load_user_image_into(
        &self,
        process: &Arc<ProcessObject>,
        install: &UserImageInstall,
    ) -> Result<(), LoadImageError>;

    /// Вставляет bootstrap-handles в child-table и создаёт первый
    /// user-поток.
    fn start_user_process(
        &self,
        process: &Arc<ProcessObject>,
        spec: UserStartSpec,
    ) -> Result<Arc<ThreadObject>, StartProcessError>;
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
