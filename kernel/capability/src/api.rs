use alloc::{sync::Arc, vec::Vec};

use collections::LockCell;
use syscall::{Rights, WakeCount};

use super::{
    errors::IpcError,
    handle::{Capability, HandleId},
    irq_line::IrqLine,
    irq_runtime::interrupts_control,
    rights::default_rights_for,
    runtime::{ParkState, runtime},
    signal::Signal,
    target::CapabilityTarget,
    wait::{CancelTarget, IndexedWaker, ParkWaker, Waitable, Waker},
};

/// Устанавливает [`Capability`] в handle-таблицу текущего процесса и
/// возвращает свежий [`HandleId`]
pub fn install_handle(handle: Capability) -> Result<HandleId, IpcError> {
    let table = runtime()
        .current_handle_table()
        .ok_or(IpcError::BadHandle)?;
    table.with_lock(|tbl| tbl.insert(handle))
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
    let runtime = runtime();
    let table = runtime.current_handle_table().ok_or(IpcError::BadHandle)?;
    let object = table.with_lock(|tbl| tbl.clone_target(handle_id, Rights::WRITE))?;
    let CapabilityTarget::Signal(signal) = &object else {
        return Err(IpcError::WrongType);
    };

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
    pub index: usize,
    pub observed: u32,
}

enum WaitSetup {
    Observed(WaitManyOutcome),
    Wait {
        signals: Vec<Arc<dyn Waitable>>,
        waker: Arc<ParkWaker>,
        indexed_wakers: Vec<Arc<IndexedWaker>>,
    },
}

enum WaitResolution {
    Signaled,
    Timeout,
    Canceled,
}

/// Ждёт пока хотя бы на одном из `items` не поднимется бит из его
/// маски. Дубликаты `HandleId` допустимы. На `timeout_ns = Some(0)` -
/// poll. `Canceled` - один из handle'ов закрыт или передан до сигнала.
pub fn signal_wait_many(
    items: &[(HandleId, u32)],
    timeout_ns: Option<u64>,
) -> Result<WaitManyOutcome, IpcError> {
    if items.is_empty() {
        return Err(IpcError::BadHandle);
    }

    let runtime = runtime();
    let table = runtime.current_handle_table().ok_or(IpcError::BadHandle)?;

    let (signals, waker, indexed_wakers) =
        match table.with_lock(|tbl| wait_many_setup(tbl, runtime, items, timeout_ns))? {
            WaitSetup::Observed(out) => return Ok(out),
            WaitSetup::Wait {
                signals,
                waker,
                indexed_wakers,
            } => (signals, waker, indexed_wakers),
        };

    runtime.block_current_until(waker.state(), timeout_ns);

    let result = match resolve_wait(&waker) {
        WaitResolution::Signaled => Ok(WaitManyOutcome {
            index: waker.winner_index() as usize,
            observed: waker.observed(),
        }),
        WaitResolution::Timeout => Err(IpcError::Timeout),
        WaitResolution::Canceled => Err(IpcError::Canceled),
    };

    for (signal, indexed) in signals.iter().zip(indexed_wakers.iter()) {
        let dyn_waker: Arc<dyn Waker> = indexed.clone();
        signal.remove_waiter(&dyn_waker);
    }
    let cancel: Arc<dyn CancelTarget> = waker;
    table.with_lock(|tbl| {
        for &(h, _) in items {
            tbl.unregister_cancel(h, &cancel);
        }
    });

    result
}

fn wait_many_setup(
    tbl: &mut crate::HandleTable,
    runtime: &Arc<dyn crate::KernelRuntime>,
    items: &[(HandleId, u32)],
    timeout_ns: Option<u64>,
) -> Result<WaitSetup, IpcError> {
    let mut signals: Vec<Arc<dyn Waitable>> = Vec::with_capacity(items.len());
    for &(h, _mask) in items {
        let obj = tbl.clone_target(h, Rights::READ)?;
        // Wait-путь не матчит тип объекта: он спрашивает у capability target его
        // ожидаемый источник. Не-ожидаемые объекты дают WrongType.
        let waitable = obj.as_waitable().ok_or(IpcError::WrongType)?;
        signals.push(waitable);
    }

    for (i, (signal, &(_h, mask))) in signals.iter().zip(items.iter()).enumerate() {
        let already = signal.peek() & mask;
        if already != 0 {
            return Ok(WaitSetup::Observed(WaitManyOutcome {
                index: i,
                observed: already,
            }));
        }
    }

    if timeout_ns == Some(0) {
        return Err(IpcError::Timeout);
    }

    let token = runtime.current_wait_token();
    let waker = Arc::new(ParkWaker::new(runtime.clone(), token));
    let mut indexed_wakers: Vec<Arc<IndexedWaker>> = Vec::with_capacity(items.len());

    for (i, (signal, &(h, mask))) in signals.iter().zip(items.iter()).enumerate() {
        let index = u32::try_from(i).expect("count ≤ u32::MAX by ABI");
        let indexed = Arc::new(IndexedWaker::new(waker.clone(), index));
        let dyn_waker: Arc<dyn Waker> = indexed.clone();
        signal.register_waiter(mask, dyn_waker);
        indexed_wakers.push(indexed);
        let cancel: Arc<dyn CancelTarget> = waker.clone();
        tbl.register_cancel(h, cancel)
            .expect("slot validated above");
    }

    Ok(WaitSetup::Wait {
        signals,
        waker,
        indexed_wakers,
    })
}

fn resolve_wait(waker: &Arc<ParkWaker>) -> WaitResolution {
    match waker.state().load(core::sync::atomic::Ordering::Acquire) {
        ParkState::SIGNALED => WaitResolution::Signaled,
        ParkState::CANCELED => WaitResolution::Canceled,
        _ => {
            if waker.claim_timeout() {
                WaitResolution::Timeout
            } else {
                match waker.state().load(core::sync::atomic::Ordering::Acquire) {
                    ParkState::CANCELED => WaitResolution::Canceled,
                    _ => WaitResolution::Signaled,
                }
            }
        }
    }
}

/// Закрывает handle и изымает его из таблицы. На несуществующем id - `BadHandle`.
/// Прав на handle не требует.
pub fn handle_close(handle_id: HandleId) -> Result<(), IpcError> {
    let table = runtime()
        .current_handle_table()
        .ok_or(IpcError::BadHandle)?;
    let removed = table.with_lock(|tbl| tbl.remove(handle_id))?;
    drop(removed);
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
    let table = runtime()
        .current_handle_table()
        .ok_or(IpcError::BadHandle)?;
    table.with_lock(|tbl| tbl.duplicate(handle_id, new_rights, new_badge))
}

/// Завершает текущий поток с заданным `exit_code`. Помечает уходящий поток
/// завершённым (на последнем потоке процесса - и процесс), поднимая `SIGNALED`
/// на их bound-`Signal`'ах, если те материализованы, и переключает контекст
/// на следующий runnable. Не возвращается.
pub fn thread_exit(exit_code: i32) -> ! {
    runtime().exit_current_thread(exit_code)
}

/// Создаёт новый [`Signal`] и регистрирует handle в таблице текущего
/// процесса. Стартовые права - [`default_rights_for`].
pub fn signal_create() -> Result<HandleId, IpcError> {
    let table = runtime()
        .current_handle_table()
        .ok_or(IpcError::BadHandle)?;
    let signal = Signal::new();
    let target = CapabilityTarget::Signal(signal);
    let handle = Capability::new(target.clone(), default_rights_for(&target));
    table.with_lock(|tbl| tbl.insert(handle))
}

// Завершение Process/Thread наблюдается ожиданием прямо по их handle через
// `CapabilityTarget::as_waitable` (bound-Signal терминации материализуется
// лениво) — отдельная op материализации Signal-хендла больше не нужна.
// Подделать событие нельзя: `signal_set` строго принимает только `Signal`.

/// Минтит [`IrqLine`] для линии `irq` по полномочию `control_handle`.
/// Требует [`Rights::WRITE`] на `IrqControl`-хендле и попадания `irq` в его
/// диапазон (`permits`).
/// Привязка линии идёт через установленный [`interrupts_control`]. Возвращает
/// handle на свежий `IrqLine` со стартовыми правами [`default_rights_for`].
pub fn irq_mint(control_handle: HandleId, irq: u16) -> Result<HandleId, IpcError> {
    let table = runtime()
        .current_handle_table()
        .ok_or(IpcError::BadHandle)?;
    let object = table.with_lock(|tbl| tbl.clone_target(control_handle, Rights::WRITE))?;
    let CapabilityTarget::IrqControl(control) = &object else {
        return Err(IpcError::WrongType);
    };
    if !control.permits(irq) {
        return Err(IpcError::AccessDenied);
    }

    let line = IrqLine::bind(interrupts_control().clone(), irq)?;
    let target = CapabilityTarget::IrqLine(line);
    let handle = Capability::new(target.clone(), default_rights_for(&target));
    table.with_lock(|tbl| tbl.insert(handle))
}

/// Подтверждает прерывание на `line_handle`: снимает latch `SIGNALED` и
/// размаскирует линию (см. [`IrqLine::ack`]). Требует [`Rights::WRITE`].
pub fn irq_ack(line_handle: HandleId) -> Result<(), IpcError> {
    let table = runtime()
        .current_handle_table()
        .ok_or(IpcError::BadHandle)?;
    let object = table.with_lock(|tbl| tbl.clone_target(line_handle, Rights::WRITE))?;
    let CapabilityTarget::IrqLine(line) = &object else {
        return Err(IpcError::WrongType);
    };
    line.ack()
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
            table_for_hook.with_lock(|tbl| tbl.remove(id).unwrap());
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
            table_for_hook.with_lock(|tbl| tbl.remove(id).unwrap());
        });
        mock().configure(Some(table), Some(hook));

        let err = signal_wait_many(&[(id, SIGNALED), (id, SIGNALED)], None).unwrap_err();
        assert_eq!(err, IpcError::Canceled);

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
