//! Мост между [`kobject`](super) и scheduler-ом.
//!
//! Чтобы IPC-функции (`object_wait_one`, future `channel_*`) не тащили
//! на каждый вызов scheduler-генерики, фиксируется единый
//! [`KernelRuntime`]-trait и `Arc<dyn KernelRuntime>` хранится в
//! глобальной [`spin::Once`] ячейке. Реализация -
//! [`super::super::sched::SchedulerHandle`], регистрируется один раз в
//! [`crate::kmain`] после bootstrap-а.

use alloc::sync::Arc;
use core::sync::atomic::AtomicU32;

use collections::MutexCell;
use drivers_common::services::scheduler::ThreadId;
use spin::Once;

use super::HandleTable;

/// Состояния парковки потока, ожидающего сигнала на KO.
///
/// `AtomicU32` атомарно сериализует исход гонки между signal-стороной
/// (поднимает [`Self::SIGNALED`]) и timeout/cancel-стороной (поднимает
/// [`Self::TIMEOUT`]). Только победитель CAS реально что-то делает -
/// проигравший заметит `state != REGISTERED` и тихо возвращается.
pub struct ParkState;

impl ParkState {
    /// Waker зарегистрирован, ни сигнал, ни timeout ещё не сработали.
    pub const REGISTERED: u32 = 0;
    /// Waker отработал по сигналу: ожидающий поток получит маску.
    pub const SIGNALED: u32 = 1;
    /// Истёк timeout либо waiter отозван - следующий wake() - no-op.
    pub const TIMEOUT: u32 = 2;
}

/// Контракт "scheduler глазами kobject".
///
/// Только эти четыре операции нужны для wait/wake-пути; намеренно не
/// расширяем интерфейс scheduler-стороной spawn/exit - те ходят через
/// `SchedulerService` без участия kobject.
pub trait KernelRuntime: Send + Sync {
    /// Идентификатор текущего потока.
    fn current_thread_id(&self) -> ThreadId;

    /// `Arc` per-process handle-table текущего потока. `None`, если
    /// scheduler ещё не bootstrapped (например, во время самой ранней
    /// инициализации) - каллер должен возвращать `IpcError::BadHandle`.
    fn current_handle_table(&self) -> Option<Arc<MutexCell<HandleTable>>>;

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
    fn unblock(&self, thread_id: ThreadId);
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
