use alloc::{sync::Arc, vec::Vec};

use collections::{LockCell, MutexCell};
use syscall::{Rights, WakeCount};

use super::{
    errors::IpcError,
    handle::{Capability, HandleId},
    handle_table::{HandleTable, RejectedInsert},
    irq_control::IrqControl,
    irq_line::IrqLine,
    irq_runtime::interrupts_control,
    runtime::runtime,
    signal::Signal,
    wait::{CancelTarget, IndexedWaker, ParkResolution, ParkWaker, Waitable, Waker},
};

/// Handle-таблица текущего процесса; `BadHandle` до инициализации планировщика.
fn current_table() -> Result<Arc<MutexCell<HandleTable>>, IpcError> {
    runtime().current_handle_table().ok_or(IpcError::BadHandle)
}

/// Устанавливает [`Capability`] в handle-таблицу текущего процесса и
/// возвращает свежий [`HandleId`]
pub fn install_handle(handle: Capability) -> Result<HandleId, IpcError> {
    current_table()?
        .with_lock(|tbl| tbl.insert(handle))
        .map_err(RejectedInsert::release)
}

/// Атомарно меняет биты сигнального состояния `Signal` (поднимает `set`,
/// снимает `clear`) и будит waiter'ов согласно [`WakeCount`].
/// Требует [`Rights::WRITE`] на handle.
pub fn signal_set(
    handle_id: HandleId,
    set: u32,
    clear: u32,
    count: WakeCount,
) -> Result<(), IpcError> {
    let signal =
        current_table()?.with_lock(|tbl| tbl.get_object::<Signal>(handle_id, Rights::WRITE))?;

    let limit = match count {
        WakeCount::None => 0,
        WakeCount::One => 1,
        WakeCount::All => usize::MAX,
    };

    signal.signal_n(set, clear, limit);

    Ok(())
}

/// Ждёт пока на capability target, к которому относится `handle_id`, не поднимется
/// хотя бы один бит из `signals`. На `timeout_ns = Some(0)` - poll.
/// `Canceled` - handle закрыт или передан до сигнала.
pub fn signal_wait_one(
    handle_id: HandleId,
    signals: u32,
    timeout_ns: Option<u64>,
) -> Result<u32, IpcError> {
    let outcome = signal_wait_many(&[(handle_id, signals)], timeout_ns)?;
    Ok(outcome.observed)
}

/// Исход [`signal_wait_many`]: индекс сработавшего item'а в `items`
/// и observed-маска на нём (пересечение с переданной маской).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitManyOutcome {
    pub index: u32,
    pub observed: u32,
}

/// Взведённое ожидание [`signal_wait_many`]: cancel-цели на слотах и waiter'ы
/// на источниках. `Drop` снимает обе регистрации.
struct ArmedWait<'a> {
    table: &'a Arc<MutexCell<HandleTable>>,
    items: &'a [(HandleId, u32)],
    sources: Vec<Arc<dyn Waitable>>,
    waker: Arc<ParkWaker>,
    indexed_wakers: Vec<Arc<IndexedWaker>>,
}

impl<'a> ArmedWait<'a> {
    /// Cancel-цели регистрируются под table-локом, waiter'ы на источниках - вне
    /// его: взведение будит уже поднятый сигнал, а пробуждение берёт
    /// scheduler-лок, который под table-локом запрещён.
    fn arm(
        table: &'a Arc<MutexCell<HandleTable>>,
        items: &'a [(HandleId, u32)],
        sources: Vec<Arc<dyn Waitable>>,
        waker: Arc<ParkWaker>,
    ) -> Result<Self, IpcError> {
        let cancel: Arc<dyn CancelTarget> = waker.clone();
        table.with_lock(|tbl| {
            for (registered, &(handle, _mask)) in items.iter().enumerate() {
                // Handle прошёл снимок источников, а к взведению исчез: закрыт или
                // передан параллельно, и исход ожидания - `Canceled`.
                if tbl.register_cancel(handle, cancel.clone()).is_err() {
                    for &(done, _mask) in &items[..registered] {
                        tbl.unregister_cancel(done, &cancel);
                    }
                    return Err(IpcError::Canceled);
                }
            }
            Ok(())
        })?;

        let mut armed = Self {
            table,
            items,
            sources,
            waker,
            indexed_wakers: Vec::with_capacity(items.len()),
        };
        for (index, (source, &(_handle, mask))) in
            (0u32..).zip(armed.sources.iter().zip(items.iter()))
        {
            let indexed = Arc::new(IndexedWaker::new(armed.waker.clone(), index));
            let dyn_waker: Arc<dyn Waker> = indexed.clone();
            source.register_waiter(mask, dyn_waker);
            armed.indexed_wakers.push(indexed);
        }
        Ok(armed)
    }
}

impl Drop for ArmedWait<'_> {
    fn drop(&mut self) {
        for (source, indexed) in self.sources.iter().zip(self.indexed_wakers.iter()) {
            let dyn_waker: Arc<dyn Waker> = indexed.clone();
            source.remove_waiter(&dyn_waker);
        }
        let cancel: Arc<dyn CancelTarget> = self.waker.clone();
        self.table.with_lock(|tbl| {
            for &(handle, _mask) in self.items {
                tbl.unregister_cancel(handle, &cancel);
            }
        });
    }
}

/// Ждёт пока хотя бы на одном из `items` не поднимется бит из его
/// маски. Дубликаты `HandleId` допустимы. На `timeout_ns = Some(0)` -
/// poll. `Canceled` - один из handle'ов закрыт или передан до сигнала.
pub fn signal_wait_many(
    items: &[(HandleId, u32)],
    timeout_ns: Option<u64>,
) -> Result<WaitManyOutcome, IpcError> {
    // Ширина индекса победителя в ABI - u32.
    if u32::try_from(items.len()).is_err() {
        return Err(IpcError::MessageTooBig);
    }
    if items.is_empty() {
        return Err(IpcError::BadHandle);
    }

    let runtime = runtime();
    let table = current_table()?;

    // Снимок источников: wait-путь не матчит тип объекта, а спрашивает у
    // capability target его ожидаемый источник. Не-ожидаемые объекты дают WrongType.
    let sources = table.with_lock(|tbl| {
        items
            .iter()
            .map(|&(handle, _mask)| {
                tbl.clone_target(handle, Rights::READ)?
                    .as_waitable()
                    .ok_or(IpcError::WrongType)
            })
            .collect::<Result<Vec<_>, IpcError>>()
    })?;

    if let Some(outcome) = peek_ready(&sources, items) {
        return Ok(outcome);
    }
    if timeout_ns == Some(0) {
        return Err(IpcError::Timeout);
    }

    let waker = Arc::new(ParkWaker::new(
        runtime.clone(),
        runtime.current_wait_token(),
    ));
    let armed = ArmedWait::arm(&table, items, sources, waker)?;

    runtime.block_current_until(armed.waker.state(), timeout_ns);

    match armed.waker.resolve() {
        ParkResolution::Signaled => Ok(WaitManyOutcome {
            index: armed.waker.winner_index(),
            observed: armed.waker.observed(),
        }),
        ParkResolution::Timeout => Err(IpcError::Timeout),
        ParkResolution::Canceled => Err(IpcError::Canceled),
    }
}

fn peek_ready(sources: &[Arc<dyn Waitable>], items: &[(HandleId, u32)]) -> Option<WaitManyOutcome> {
    (0u32..).zip(sources.iter().zip(items.iter())).find_map(
        |(index, (source, &(_handle, mask)))| {
            let observed = source.peek() & mask;
            (observed != 0).then_some(WaitManyOutcome { index, observed })
        },
    )
}

/// Закрывает handle и изымает его из таблицы. На несуществующем id - `BadHandle`.
/// Прав на handle не требует.
pub fn handle_close(handle_id: HandleId) -> Result<(), IpcError> {
    current_table()?
        .with_lock(|tbl| tbl.remove(handle_id))?
        .release();
    Ok(())
}

/// Создаёт новый handle на тот же capability target с подмножеством прав и (опционально)
/// значком (badge). Требует [`Rights::DUPLICATE`] на исходном handle.
/// Семантика значка - set-once: заклеймить можно только незаклеймённый хендл; заклеймённый
/// наследует значок, переклеймить нельзя (см. [`Capability::duplicate`]).
pub fn handle_duplicate(
    handle_id: HandleId,
    new_rights: Rights,
    new_badge: u64,
) -> Result<HandleId, IpcError> {
    current_table()?
        .with_lock(|tbl| tbl.duplicate(handle_id, new_rights, new_badge))
        .map_err(RejectedInsert::release)
}

/// Завершает текущий поток с заданным `exit_code`. Помечает уходящий поток
/// завершённым (на последнем потоке процесса - и процесс), поднимая `SIGNALED`
/// на их bound-`Signal`'ах, если те материализованы, и переключает контекст
/// на следующий runnable. Не возвращается.
pub fn thread_exit(exit_code: i32) -> ! {
    runtime().exit_current_thread(exit_code)
}

/// Создаёт новый [`Signal`] и регистрирует handle в таблице текущего
/// процесса. Стартовые права - [`default_rights_for`](super::rights::default_rights_for).
pub fn signal_create() -> Result<HandleId, IpcError> {
    install_handle(Capability::new_with_default_rights(Signal::new()))
}

// Завершение Process/Thread наблюдается ожиданием прямо по их handle через
// `CapabilityTarget::as_waitable` (bound-Signal терминации материализуется
// лениво) — отдельная op материализации Signal-хендла больше не нужна.
// Подделать событие нельзя: `signal_set` строго принимает только `Signal`.

/// Минтит [`IrqLine`] для линии `irq` по полномочию `control_handle`.
/// Требует [`Rights::WRITE`] на `IrqControl`-хендле и попадания `irq` в его
/// диапазон (`permits`).
/// Привязка линии идёт через установленный [`interrupts_control`]. Возвращает
/// handle на свежий `IrqLine` со стартовыми правами
/// [`default_rights_for`](super::rights::default_rights_for).
pub fn irq_mint(control_handle: HandleId, irq: u16) -> Result<HandleId, IpcError> {
    let table = current_table()?;
    let control =
        table.with_lock(|tbl| tbl.get_object::<IrqControl>(control_handle, Rights::WRITE))?;
    if !control.permits(irq) {
        return Err(IpcError::AccessDenied);
    }

    let line = IrqLine::bind(interrupts_control().clone(), irq)?;
    let handle = Capability::new_with_default_rights(line);
    table
        .with_lock(|tbl| tbl.insert(handle))
        .map_err(RejectedInsert::release)
}

/// Подтверждает прерывание на `line_handle`: снимает latch `SIGNALED` и
/// размаскирует линию (см. [`IrqLine::ack`]). Требует [`Rights::WRITE`].
pub fn irq_ack(line_handle: HandleId) -> Result<(), IpcError> {
    current_table()?
        .with_lock(|tbl| tbl.get_object::<IrqLine>(line_handle, Rights::WRITE))?
        .ack()
}

#[cfg(test)]
mod tests {
    use core::{
        num::NonZeroU64,
        sync::atomic::{AtomicI32, AtomicU32, Ordering},
    };
    use std::{
        panic,
        sync::{Mutex, OnceLock},
    };

    use collections::MutexCell;

    use super::*;
    use crate::{
        HandleTable, ProcessObject, Rights, Signal, ThreadObject,
        handle::Capability,
        runtime::{KernelRuntime, WaitToken, install_runtime},
        signal::SIGNALED,
        target::CapabilityTarget,
    };

    const PANIC_SENTINEL: &str = "thread_exit-mock-noreturn";
    static CAPTURED_EXIT_CODE: AtomicI32 = AtomicI32::new(i32::MIN);

    type BlockHook = Box<dyn FnMut() + Send>;

    struct MockRuntime {
        handle_table: Mutex<Option<Arc<MutexCell<HandleTable>>>>,
        block_hook: Mutex<Option<BlockHook>>,
    }

    impl MockRuntime {
        fn new() -> Self {
            Self {
                handle_table: Mutex::new(None),
                block_hook: Mutex::new(None),
            }
        }

        fn configure(
            &self,
            handle_table: Option<Arc<MutexCell<HandleTable>>>,
            block_hook: Option<BlockHook>,
        ) {
            *self.handle_table.lock().unwrap() = handle_table;
            *self.block_hook.lock().unwrap() = block_hook;
        }

        fn reset(&self) {
            self.configure(None, None);
        }
    }

    impl KernelRuntime for MockRuntime {
        fn current_wait_token(&self) -> WaitToken {
            WaitToken::new(NonZeroU64::new(1).unwrap())
        }
        fn now_ns(&self) -> u64 {
            0
        }

        fn current_handle_table(&self) -> Option<Arc<MutexCell<HandleTable>>> {
            self.handle_table.lock().unwrap().clone()
        }

        fn exit_current_thread(&self, exit_code: i32) -> ! {
            CAPTURED_EXIT_CODE.store(exit_code, Ordering::SeqCst);
            panic!("{PANIC_SENTINEL}");
        }

        fn block_current_until(&self, _ready_flag: &AtomicU32, _timeout_ns: Option<u64>) {
            if let Some(hook) = self.block_hook.lock().unwrap().as_mut() {
                hook();
            }
        }

        fn unblock(&self, _token: WaitToken) {}

        fn set_blocked_cancel(&self, _cancel: Arc<dyn crate::CancelTarget>) {}

        fn clear_blocked_cancel(&self) {}
    }

    /// install_runtime - once per process, поэтому singleton
    /// с переключаемой конфигурацией. test_lock сериализует тесты,
    /// чтобы block_hook не перетирался между ними.
    fn mock() -> &'static MockRuntime {
        static MOCK: OnceLock<Arc<MockRuntime>> = OnceLock::new();
        MOCK.get_or_init(|| {
            let rt = Arc::new(MockRuntime::new());
            install_runtime(rt.clone() as Arc<dyn KernelRuntime>);
            rt
        })
    }

    fn test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn install_signal_handle(
        rights: Rights,
    ) -> (Arc<MutexCell<HandleTable>>, HandleId, Arc<Signal>) {
        let table = Arc::new(MutexCell::new(HandleTable::new()));
        let signal = Signal::new();
        let id = table
            .with_lock(|tbl| {
                tbl.insert(Capability::new(
                    CapabilityTarget::Signal(signal.clone()),
                    rights,
                ))
            })
            .unwrap();
        (table, id, signal)
    }

    #[test]
    fn thread_exit_forwards_code_to_runtime() {
        let _guard = test_lock();
        mock().reset();

        let prev_hook = panic::take_hook();
        panic::set_hook(Box::new(|_| {}));
        let result = panic::catch_unwind(|| thread_exit(7));
        panic::set_hook(prev_hook);

        assert!(result.is_err());
        assert_eq!(CAPTURED_EXIT_CODE.load(Ordering::SeqCst), 7);
    }

    #[test]
    fn signal_wait_one_returns_latched_observed_after_signal_state_cleared() {
        let _guard = test_lock();
        let (table, id, signal) = install_signal_handle(Rights::READ | Rights::WRITE);
        let signal_for_hook = signal.clone();
        let hook: BlockHook = Box::new(move || {
            signal_for_hook.signal(SIGNALED, 0);
            signal_for_hook.signal(0, SIGNALED);
        });
        mock().configure(Some(table), Some(hook));

        let observed = signal_wait_one(id, SIGNALED, None).expect("Ok");
        assert_eq!(observed & SIGNALED, SIGNALED);
        assert_eq!(signal.peek() & SIGNALED, 0);

        mock().reset();
    }

    #[test]
    fn signal_wait_many_returns_latched_outcome_for_winning_item() {
        let _guard = test_lock();
        let (table, id_a, _ev_a) = install_signal_handle(Rights::READ | Rights::WRITE);
        let signal_b = Signal::new();
        let id_b = table
            .with_lock(|tbl| {
                tbl.insert(Capability::new(
                    CapabilityTarget::Signal(signal_b.clone()),
                    Rights::READ | Rights::WRITE,
                ))
            })
            .unwrap();

        let signal_b_for_hook = signal_b.clone();
        let hook: BlockHook = Box::new(move || {
            signal_b_for_hook.signal(SIGNALED, 0);
            signal_b_for_hook.signal(0, SIGNALED);
        });
        mock().configure(Some(table), Some(hook));

        let outcome = signal_wait_many(&[(id_a, SIGNALED), (id_b, SIGNALED)], None).expect("Ok");
        assert_eq!(outcome.index, 1);
        assert_eq!(outcome.observed & SIGNALED, SIGNALED);
        assert_eq!(signal_b.peek() & SIGNALED, 0);

        mock().reset();
    }

    #[test]
    fn signal_wait_one_returns_canceled_when_handle_closed_during_wait() {
        let _guard = test_lock();
        let (table, id, _signal) = install_signal_handle(Rights::READ);
        let table_for_hook = table.clone();
        let hook: BlockHook = Box::new(move || {
            table_for_hook
                .with_lock(|tbl| tbl.remove(id).unwrap())
                .release();
        });
        mock().configure(Some(table), Some(hook));

        let err = signal_wait_one(id, SIGNALED, None).unwrap_err();
        assert_eq!(err, IpcError::Canceled);

        mock().reset();
    }

    #[test]
    fn signal_wait_many_canceled_when_any_handle_closed_during_wait() {
        let _guard = test_lock();
        let (table, id, _signal) = install_signal_handle(Rights::READ);
        let table_for_hook = table.clone();
        let hook: BlockHook = Box::new(move || {
            table_for_hook
                .with_lock(|tbl| tbl.remove(id).unwrap())
                .release();
        });
        mock().configure(Some(table), Some(hook));

        let err = signal_wait_many(&[(id, SIGNALED), (id, SIGNALED)], None).unwrap_err();
        assert_eq!(err, IpcError::Canceled);

        mock().reset();
    }

    /// Regression guard: handle, исчезнувший между снимком источников и
    /// взведением, закрывает ожидание тем же `Canceled`, что и уход во сне.
    #[test]
    fn arming_a_wait_on_a_vanished_handle_is_canceled() {
        let _guard = test_lock();
        let (table, id, signal) = install_signal_handle(Rights::READ);
        mock().configure(Some(table.clone()), None);
        table.with_lock(|tbl| tbl.remove(id).unwrap()).release();

        let sources: Vec<Arc<dyn Waitable>> = alloc::vec![signal];
        let runtime = runtime();
        let waker = Arc::new(ParkWaker::new(
            runtime.clone(),
            runtime.current_wait_token(),
        ));
        let items = [(id, SIGNALED)];
        let armed = ArmedWait::arm(&table, &items, sources, waker);

        assert!(matches!(armed.err(), Some(IpcError::Canceled)));

        mock().reset();
    }

    struct CountingWaker {
        woken: AtomicU32,
    }

    impl Waker for CountingWaker {
        fn wake(&self, _observed: u32) {
            self.woken.fetch_add(1, Ordering::AcqRel);
        }
    }

    impl CountingWaker {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                woken: AtomicU32::new(0),
            })
        }

        fn was_woken(&self) -> bool {
            self.woken.load(Ordering::Acquire) != 0
        }
    }

    #[test]
    fn signal_set_with_count_limits_wakeups() {
        let _guard = test_lock();
        let (table, id, signal) = install_signal_handle(Rights::WRITE);
        mock().configure(Some(table), None);

        let w1 = CountingWaker::new();
        let w2 = CountingWaker::new();
        signal.register_waiter(SIGNALED, w1.clone());
        signal.register_waiter(SIGNALED, w2.clone());

        signal_set(id, SIGNALED, 0, WakeCount::One).expect("signal ok");
        assert!(w1.was_woken());
        assert!(!w2.was_woken());

        mock().reset();
    }

    #[test]
    fn signal_set_with_all_wakes_all() {
        let _guard = test_lock();
        let (table, id, signal) = install_signal_handle(Rights::WRITE);
        mock().configure(Some(table), None);

        let w1 = CountingWaker::new();
        let w2 = CountingWaker::new();
        signal.register_waiter(SIGNALED, w1.clone());
        signal.register_waiter(SIGNALED, w2.clone());

        signal_set(id, SIGNALED, 0, WakeCount::All).expect("signal ok");
        assert!(w1.was_woken());
        assert!(w2.was_woken());

        mock().reset();
    }

    #[test]
    fn signal_set_with_none_wakes_nobody_but_sets_bits() {
        let _guard = test_lock();
        let (table, id, signal) = install_signal_handle(Rights::WRITE);
        mock().configure(Some(table), None);

        let w1 = CountingWaker::new();
        signal.register_waiter(SIGNALED, w1.clone());

        signal_set(id, SIGNALED, 0, WakeCount::None).expect("signal ok");
        assert!(!w1.was_woken());
        assert_eq!(signal.peek() & SIGNALED, SIGNALED);

        mock().reset();
    }

    #[test]
    fn signal_set_on_non_signal_object_is_wrong_type() {
        let _guard = test_lock();
        let table = Arc::new(MutexCell::new(HandleTable::new()));
        let id = table
            .with_lock(|tbl| {
                tbl.insert(Capability::new(
                    CapabilityTarget::Process(ProcessObject::new()),
                    Rights::WRITE,
                ))
            })
            .unwrap();
        mock().configure(Some(table), None);

        assert_eq!(
            signal_set(id, SIGNALED, 0, WakeCount::All),
            Err(IpcError::WrongType)
        );

        mock().reset();
    }

    #[test]
    fn signal_set_without_write_right_is_access_denied() {
        let _guard = test_lock();
        // Signal-handle только с READ: clone_target(WRITE) -> AccessDenied.
        let (table, id, _signal) = install_signal_handle(Rights::READ);
        mock().configure(Some(table), None);

        assert_eq!(
            signal_set(id, SIGNALED, 0, WakeCount::All),
            Err(IpcError::AccessDenied)
        );

        mock().reset();
    }

    #[test]
    fn signal_wait_many_empty_slice_is_bad_handle() {
        let _guard = test_lock();
        let table = Arc::new(MutexCell::new(HandleTable::new()));
        mock().configure(Some(table), None);

        assert_eq!(
            signal_wait_many(&[], None).map(|_| ()),
            Err(IpcError::BadHandle)
        );

        mock().reset();
    }

    #[test]
    fn signal_wait_poll_times_out_when_no_signal_pending() {
        let _guard = test_lock();
        // timeout_ns == Some(0): poll-путь. Сигнал не поднят -> Timeout,
        // block_current_until не вызывается (hook отсутствует).
        let (table, id, _signal) = install_signal_handle(Rights::READ);
        mock().configure(Some(table), None);

        assert_eq!(
            signal_wait_one(id, SIGNALED, Some(0)),
            Err(IpcError::Timeout)
        );

        mock().reset();
    }

    #[test]
    fn signal_wait_poll_returns_already_pending_signal() {
        let _guard = test_lock();
        // timeout_ns == Some(0), но бит уже поднят -> наблюдаем сразу, без блока.
        let (table, id, signal) = install_signal_handle(Rights::READ);
        signal.signal(SIGNALED, 0);
        mock().configure(Some(table), None);

        let observed = signal_wait_one(id, SIGNALED, Some(0)).expect("observed pending");
        assert_eq!(observed & SIGNALED, SIGNALED);

        mock().reset();
    }

    #[test]
    fn signal_wait_one_on_process_handle_wakes_on_terminate() {
        // Прямое ожидание по Process-хендлу через as_waitable: материализует
        // bound-Signal терминации и просыпается на signal_terminated.
        let _guard = test_lock();
        let table = Arc::new(MutexCell::new(HandleTable::new()));
        let process = ProcessObject::new();
        let id = table
            .with_lock(|tbl| {
                tbl.insert(Capability::new(
                    CapabilityTarget::Process(process.clone()),
                    Rights::READ,
                ))
            })
            .unwrap();
        let process_for_hook = process.clone();
        let hook: BlockHook = Box::new(move || process_for_hook.signal_terminated(0));
        mock().configure(Some(table), Some(hook));

        let observed = signal_wait_one(id, SIGNALED, None).expect("Ok");
        assert_eq!(observed & SIGNALED, SIGNALED);

        mock().reset();
    }

    #[test]
    fn signal_wait_one_on_thread_handle_wakes_on_terminate() {
        let _guard = test_lock();
        let table = Arc::new(MutexCell::new(HandleTable::new()));
        let thread = ThreadObject::new();
        let id = table
            .with_lock(|tbl| {
                tbl.insert(Capability::new(
                    CapabilityTarget::Thread(thread.clone()),
                    Rights::READ,
                ))
            })
            .unwrap();
        let thread_for_hook = thread.clone();
        let hook: BlockHook = Box::new(move || thread_for_hook.signal_terminated(0));
        mock().configure(Some(table), Some(hook));

        let observed = signal_wait_one(id, SIGNALED, None).expect("Ok");
        assert_eq!(observed & SIGNALED, SIGNALED);

        mock().reset();
    }

    #[test]
    fn signal_wait_one_on_non_waitable_object_is_wrong_type() {
        // Port не ожидаем: as_waitable -> None -> WrongType (без блокировки).
        let _guard = test_lock();
        let table = Arc::new(MutexCell::new(HandleTable::new()));
        let id = table
            .with_lock(|tbl| {
                tbl.insert(Capability::new(
                    CapabilityTarget::Port(crate::Port::new()),
                    Rights::READ,
                ))
            })
            .unwrap();
        mock().configure(Some(table), None);

        assert_eq!(
            signal_wait_one(id, SIGNALED, Some(0)).map(|_| ()),
            Err(IpcError::WrongType)
        );

        mock().reset();
    }

    #[test]
    fn signal_set_on_process_handle_cannot_forge_termination() {
        // Security-инвариант (теперь структурный): даже с WRITE на Process-хендле
        // нельзя поднять SIGNALED на bound-Signal терминации — signal_set строго
        // принимает только Signal, а наружу Signal-таргет терминации не выдаётся.
        let _guard = test_lock();
        let table = Arc::new(MutexCell::new(HandleTable::new()));
        let id = table
            .with_lock(|tbl| {
                tbl.insert(Capability::new(
                    CapabilityTarget::Process(ProcessObject::new()),
                    Rights::WRITE,
                ))
            })
            .unwrap();
        mock().configure(Some(table), None);

        assert_eq!(
            signal_set(id, SIGNALED, 0, WakeCount::All),
            Err(IpcError::WrongType)
        );

        mock().reset();
    }

    #[test]
    fn signal_create_inserts_handle_with_signal_and_wait_rights() {
        let _guard = test_lock();
        let table = Arc::new(MutexCell::new(HandleTable::new()));
        mock().configure(Some(table.clone()), None);

        let id = signal_create().expect("signal_create ok");
        let rights = table
            .with_lock(|tbl| {
                tbl.get(id, Rights::WRITE | Rights::READ)
                    .map(Capability::rights)
            })
            .expect("handle present with SIGNAL|WAIT");
        assert!(rights.contains(Rights::WRITE));
        assert!(rights.contains(Rights::READ));

        mock().reset();
    }

    /// Ставит тривиальный глобальный контроллер один раз: bind_line всегда
    /// успешен, mask/unmask — no-op. Протокол fire/ack проверяется отдельно в
    /// `irq_line` с записывающим моком; здесь — только гейтинг минта.
    fn ensure_irq_control() {
        use std::sync::OnceLock;
        static INSTALLED: OnceLock<()> = OnceLock::new();
        INSTALLED.get_or_init(|| {
            struct StubControl;
            impl crate::InterruptsControl for StubControl {
                fn bind_line(
                    &self,
                    _irq: u16,
                    _sink: Arc<dyn crate::IrqSink>,
                ) -> Result<crate::IrqBindToken, IpcError> {
                    Ok(crate::IrqBindToken::new(()))
                }
                fn mask(&self, _irq: u16) {}
                fn unmask(&self, _irq: u16) {}
            }
            crate::install_interrupts_control(Arc::new(StubControl));
        });
    }

    fn irq_control_handle(
        rights: Rights,
        lo: u16,
        hi: u16,
    ) -> (Arc<MutexCell<HandleTable>>, HandleId) {
        let table = Arc::new(MutexCell::new(HandleTable::new()));
        let id = table
            .with_lock(|tbl| {
                tbl.insert(Capability::new(
                    CapabilityTarget::IrqControl(crate::IrqControl::new(lo, hi)),
                    rights,
                ))
            })
            .unwrap();
        (table, id)
    }

    #[test]
    fn irq_mint_returns_irq_line_handle() {
        let _guard = test_lock();
        ensure_irq_control();
        let (table, ctl_id) = irq_control_handle(Rights::WRITE, 32, 64);
        mock().configure(Some(table.clone()), None);

        let line_id = irq_mint(ctl_id, 40).expect("mint ok");
        let is_line = table.with_lock(|tbl| {
            matches!(
                tbl.clone_target(line_id, Rights::READ),
                Ok(CapabilityTarget::IrqLine(_))
            )
        });
        assert!(is_line, "minted handle must be an IrqLine");

        mock().reset();
    }

    #[test]
    fn irq_mint_out_of_band_is_access_denied() {
        let _guard = test_lock();
        ensure_irq_control();
        let (table, ctl_id) = irq_control_handle(Rights::WRITE, 32, 64);
        mock().configure(Some(table), None);

        assert_eq!(irq_mint(ctl_id, 100), Err(IpcError::AccessDenied));

        mock().reset();
    }

    #[test]
    fn irq_mint_without_write_is_access_denied() {
        let _guard = test_lock();
        ensure_irq_control();
        let (table, ctl_id) = irq_control_handle(Rights::READ, 32, 64);
        mock().configure(Some(table), None);

        assert_eq!(irq_mint(ctl_id, 40), Err(IpcError::AccessDenied));

        mock().reset();
    }

    #[test]
    fn irq_mint_on_non_control_is_wrong_type() {
        let _guard = test_lock();
        ensure_irq_control();
        let (table, id, _sig) = install_signal_handle(Rights::WRITE);
        mock().configure(Some(table), None);

        assert_eq!(irq_mint(id, 40), Err(IpcError::WrongType));

        mock().reset();
    }

    #[test]
    fn irq_ack_on_armed_line_is_wrong_type() {
        let _guard = test_lock();
        ensure_irq_control();
        let (table, ctl_id) = irq_control_handle(Rights::WRITE, 32, 64);
        mock().configure(Some(table), None);

        let line_id = irq_mint(ctl_id, 40).expect("mint ok");
        // Свежеминченная линия в ARMED — подтверждать нечего.
        assert_eq!(irq_ack(line_id), Err(IpcError::WrongType));

        mock().reset();
    }
}
