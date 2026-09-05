//! Мост между capability и рантаймом ядра.

use alloc::sync::Arc;
use core::{num::NonZeroU64, sync::atomic::AtomicU32};

use collections::{MutexCell, StaticString};
use spin::Once;
use syscall::MAX_PROCESS_NAME_LEN;

use super::{HandleTable, wait::CancelTarget};

/// Параметры первого входа в user-поток.
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

/// Состояния парковки потока, ожидающего сигнала на capability target.
pub struct ParkState;

impl ParkState {
    pub const REGISTERED: u32 = 0;
    pub const SIGNALED: u32 = 1;
    pub const TIMEOUT: u32 = 2;
    pub const CANCELED: u32 = 3;
}

/// Opaque token текущего execution context для wait/wake-пути.
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

/// Операции планировщика, нужные механизму capability: чтение текущего
/// контекста (wait-token, handle-таблица) и парковка/пробуждение потока.
pub trait KernelRuntime: Send + Sync {
    /// Opaque token текущего потока/контекста.
    fn current_wait_token(&self) -> WaitToken;

    /// Монотонное время ядра (нс) - источник тот же, что у дедлайнов
    /// [`block_current_until`](Self::block_current_until).
    fn now_ns(&self) -> u64;

    /// `Arc` per-process handle-таблицы текущего потока. `None`, если
    /// планировщик ещё не инициализирован.
    fn current_handle_table(&self) -> Option<Arc<MutexCell<HandleTable>>>;

    /// Имя текущего процесса для диагностики. `None`, если рантайм имён
    /// не ведёт.
    fn current_process_name(&self) -> Option<StaticString<MAX_PROCESS_NAME_LEN>> {
        None
    }

    /// Завершает текущий поток с заданным `exit_code`: поднимает
    /// terminated-флаг потока на `Arc<ThreadObject>`, при последнем
    /// потоке процесса - terminated-флаг процесса, и переключает контекст
    /// на следующий runnable. Не возвращается.
    fn exit_current_thread(&self, exit_code: i32) -> !;

    /// Завершает все потоки текущего процесса с заданным `exit_code`,
    /// освобождает его ресурсы и переключает контекст на следующий runnable.
    /// Не возвращается.
    fn exit_current_process(&self, exit_code: i32) -> !;

    /// Уступает остаток кванта: текущий поток встаёт в конец своей
    /// ready-очереди и остаётся runnable.
    fn yield_current(&self);

    /// Паркует текущий поток.
    /// `timeout_ns = None` означает бессрочный wait, `Some(d)` -
    /// wakeup-deadline через тот же `SleepQueue`, что и `sleep_ns`.
    fn block_current_until(&self, ready_flag: &AtomicU32, timeout_ns: Option<u64>);

    /// Будит ранее заблокированный поток: переводит в `Ready` и помещает
    /// в ready_queue.
    fn unblock(&self, token: WaitToken);

    /// Привязывает к текущему потоку cancel-хук.
    fn set_blocked_cancel(&self, cancel: Arc<dyn CancelTarget>);

    /// Снимает cancel-хук блокировки с текущего потока.
    fn clear_blocked_cancel(&self);
}

static RUNTIME: Once<Arc<dyn KernelRuntime>> = Once::new();

/// Устанавливает глобальный runtime.
pub fn install_runtime(rt: Arc<dyn KernelRuntime>) {
    assert!(
        RUNTIME.get().is_none(),
        "KernelRuntime is already installed"
    );
    let _ = RUNTIME.call_once(|| rt);
}

/// Доступ к глобальному runtime. Паникует, если он ещё не установлен.
#[allow(clippy::expect_used)]
pub fn runtime() -> &'static Arc<dyn KernelRuntime> {
    // Runtime ставится до запуска планировщика, а вызывающие исполняются на его
    // потоках, и отсутствие runtime здесь означает сломанный порядок загрузки.
    RUNTIME
        .get()
        .expect("KernelRuntime not installed; install_runtime() must be called in kmain")
}

pub(crate) fn installed_runtime() -> Option<&'static Arc<dyn KernelRuntime>> {
    RUNTIME.get()
}
