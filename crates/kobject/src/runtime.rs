//! Мост между kobject и runtime-ом ядра.
//!
//! Чтобы IPC-функции (`object_wait_one`, `channel_*`) не тащили на
//! каждый вызов scheduler-генерики, фиксируется единый
//! [`KernelRuntime`]-trait и `Arc<dyn KernelRuntime>` хранится в
//! глобальной [`spin::Once`] ячейке. Реализация регистрируется один раз
//! после bootstrap-а scheduler-а.

use alloc::sync::Arc;
use core::{num::NonZeroU64, sync::atomic::AtomicU32};

use collections::MutexCell;
use spin::Once;

use super::{
    HandleTable, IpcError, ProcessObject, SpawnError, ThreadObject,
    spawn::{LoadImageError, StartProcessError, UserImageInstall, UserStartSpec},
};

/// Параметры первого входа в user-поток, передаваемые в
/// [`KernelRuntime::create_user_thread`]. Платформенно-нейтральное описание;
/// реализация преобразует поля в формат архитектурного контекста.
#[derive(Debug, Clone, Copy)]
pub struct UserThreadEntry {
    /// Стартовый адрес user-кода.
    pub entry_pc: u64,
    /// Стартовый user-стек.
    pub user_sp: u64,
    /// Аргумент, передаваемый user-коду через ABI.
    pub arg: u64,
    /// Приоритет нового потока (0 - высший).
    pub priority: u8,
}

/// Состояния парковки потока, ожидающего сигнала на KO. CAS на
/// `AtomicU32` сериализует гонку signal/timeout/cancel - публикует
/// исход ровно один победитель.
pub struct ParkState;

impl ParkState {
    pub const REGISTERED: u32 = 0;
    pub const SIGNALED: u32 = 1;
    pub const TIMEOUT: u32 = 2;
    pub const CANCELED: u32 = 3;
}

/// Opaque token текущего execution context для wait/wake-пути.
///
/// Значение выдаёт scheduler runtime. kobject хранит и возвращает token,
/// но не интерпретирует его биты.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct WaitToken(NonZeroU64);

impl WaitToken {
    pub const fn new(raw: NonZeroU64) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> NonZeroU64 {
        self.0
    }
}

/// Контракт "scheduler глазами kobject".
///
/// Только эти операции нужны kobject wait/wake-пути; user-VM и syscall
/// runtime живут в своих слоях.
pub trait KernelRuntime: Send + Sync {
    /// Opaque token текущего потока/контекста.
    fn current_wait_token(&self) -> WaitToken;

    /// `Arc` per-process handle-table текущего потока. `None`, если
    /// scheduler ещё не bootstrapped (например, во время самой ранней
    /// инициализации) - каллер должен возвращать `IpcError::BadHandle`.
    fn current_handle_table(&self) -> Option<Arc<MutexCell<HandleTable>>>;

    /// `Arc<ThreadObject>` текущего потока. `None`, если scheduler ещё
    /// не bootstrapped и current-thread не определён.
    fn current_thread_object(&self) -> Option<Arc<ThreadObject>>;

    /// `Arc<ProcessObject>` процесса, к которому привязан текущий поток.
    /// `None`, если scheduler ещё не bootstrapped и current-process не
    /// определён.
    fn current_process_object(&self) -> Option<Arc<ProcessObject>>;

    /// Завершает текущий поток с заданным `exit_code`: поднимает
    /// `THREAD_TERMINATED` на `Arc<ThreadObject>`, при последнем
    /// потоке процесса - `PROCESS_TERMINATED`, и переключает контекст
    /// на следующий runnable. Не возвращается.
    fn exit_current_thread(&self, exit_code: i32) -> !;

    /// Парк current thread. Под scheduler-lock-ом проверяется
    /// `ready_flag`: если он уже не [`ParkState::REGISTERED`], значит
    /// waker успел отработать между `register_waiter` и взятием
    /// scheduler-lock - блокировка пропускается.
    ///
    /// `timeout_ns = None` означает бессрочный wait, `Some(d)` -
    /// wakeup-deadline через тот же `SleepQueue`, что и `sleep_ns`.
    fn block_current_until(&self, ready_flag: &AtomicU32, timeout_ns: Option<u64>);

    /// Будит ранее заблокированный поток: переводит в `Ready` и пихает
    /// в ready_queue. Idempotent на любых других состояниях потока.
    fn unblock(&self, token: WaitToken);

    /// Создаёт пустой user-процесс: новое адресное пространство, пустая
    /// handle-table, ноль потоков и сохранённое имя процесса.
    fn create_empty_process(&self, name: &str) -> Result<Arc<ProcessObject>, SpawnError>;

    /// Создаёт user-поток в указанном процессе и помещает его в ready-queue.
    /// На успехе процесс получает инкрементированный thread_count.
    fn create_user_thread(
        &self,
        process: &Arc<ProcessObject>,
        entry: UserThreadEntry,
    ) -> Result<Arc<ThreadObject>, SpawnError>;

    /// Идемпотентно завершает поток: поднимает `THREAD_TERMINATED`,
    /// декрементирует thread_count процесса; на нуле - поднимает
    /// `PROCESS_TERMINATED` владеющего процесса. Не выполняет context
    /// switch: завершение собственного потока должно идти через
    /// [`Self::exit_current_thread`].
    fn terminate_thread(&self, thread: &Arc<ThreadObject>, exit_code: i32) -> Result<(), IpcError>;

    /// Идемпотентно завершает все потоки процесса: для каждого живого
    /// потока поднимает `THREAD_TERMINATED`, по достижении нуля -
    /// `PROCESS_TERMINATED`. Не выполняет context switch.
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

static RUNTIME: Once<Arc<dyn KernelRuntime>> = Once::new();

/// Устанавливает глобальный runtime. Должен вызываться ровно один раз
/// после bootstrap-а scheduler-а; повторная установка - bug в порядке
/// инициализации, `panic`.
pub fn install_runtime(rt: Arc<dyn KernelRuntime>) {
    assert!(
        RUNTIME.get().is_none(),
        "KernelRuntime is already installed"
    );
    let _ = RUNTIME.call_once(|| rt);
}

/// Доступ к глобальному runtime; `panic`, если он ещё не установлен.
pub fn runtime() -> &'static Arc<dyn KernelRuntime> {
    RUNTIME
        .get()
        .expect("KernelRuntime not installed; install_runtime() must be called in kmain")
}
