use alloc::{
    sync::{Arc, Weak},
    vec::Vec,
};

use kbase::{LockCell, MutexCell, StaticString};
use syscall::{IrqTrigger, MAX_PROCESS_NAME_LEN, Rights, WakeCount};

use super::{
    errors::IpcError,
    handle::{Capability, HandleId},
    handle_table::{HandleTable, RejectedInsert},
    irq_control::IrqControl,
    irq_line::IrqLine,
    rev_node::RevocationHook,
    runtime::{installed_runtime, runtime},
    signal::Signal,
    wait::{
        CancelTarget, IndexedWaker, ParkResolution, ParkWaker, Waitable, Waker, deadline_after,
        park_until_deadline,
    },
};

fn current_table() -> Result<Arc<MutexCell<HandleTable>>, IpcError> {
    runtime().current_handle_table().ok_or(IpcError::BadHandle)
}

/// Устанавливает [`Capability`] в handle-таблицу текущего процесса и возвращает свежий
/// [`HandleId`].
///
/// Владение `handle` переходит таблице. На исчерпании таблицы объект разрушается здесь же, и
/// вызывающий получает [`IpcError::OutOfHandles`]. Пока планировщик не отдаёт таблицу текущего
/// потока - [`IpcError::BadHandle`].
pub fn install_handle(handle: Capability) -> Result<HandleId, IpcError> {
    current_table()?
        .with_lock(|tbl| tbl.insert(handle))
        .map_err(RejectedInsert::release)
}

/// Атомарно меняет биты сигнального состояния `Signal` (поднимает `set`,
/// снимает `clear`) и будит ожидающих согласно [`WakeCount`].
/// Требует [`Rights::WRITE`] на handle.
///
/// Бит, названный в обеих масках, остаётся поднятым. Биты применяются при любом `count`, и
/// [`WakeCount::None`] лишь оставляет ожидающих в списке. Хендл другого вида даёт `WrongType`.
/// Событие терминации процесса либо потока этой операцией не подделывается.
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
/// хотя бы один бит из `signals`. Возвращает наблюдённые биты в пересечении с `signals`.
///
/// Требует [`Rights::READ`]. Ожидание применимо к `Signal`, `Process`, `Thread` и `IrqLine`,
/// прочие виды объекта отвергаются с `WrongType`.
///
/// Значения `timeout_ns`:
///  - `None` - ждать бессрочно,
///  - `Some(0)` - опрос без парковки, пустой опрос отвечает `Timeout`,
///  - прочее - относительный дедлайн.
///
/// Handle, закрытый либо переданный до прихода сигнала, снимает ожидание кодом `Canceled`.
pub fn signal_wait_one(
    handle_id: HandleId,
    signals: u32,
    timeout_ns: Option<u64>,
) -> Result<u32, IpcError> {
    let outcome = signal_wait_many(&[(handle_id, signals)], timeout_ns)?;
    Ok(outcome.observed)
}

/// Исход [`signal_wait_many`]: индекс сработавшего item в `items`
/// и observed-маска на нём (пересечение с переданной маской).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitManyOutcome {
    pub index: u32,
    pub observed: u32,
}

struct SignalWaitState {
    handles: usize,
    sources: usize,
    canceled: bool,
}

struct SignalWaitCancel {
    table: Arc<MutexCell<HandleTable>>,
    handles: Vec<HandleId>,
    sources: Vec<Arc<dyn Waitable>>,
    waker: Arc<ParkWaker>,
    indexed_wakers: Vec<Arc<IndexedWaker>>,
    state: MutexCell<SignalWaitState>,
}

impl SignalWaitCancel {
    fn new(
        table: Arc<MutexCell<HandleTable>>,
        items: &[(HandleId, u32)],
        sources: Vec<Arc<dyn Waitable>>,
        waker: Arc<ParkWaker>,
    ) -> Arc<Self> {
        let indexed_wakers = (0u32..)
            .zip(items.iter())
            .map(|(index, _)| Arc::new(IndexedWaker::new(waker.clone(), index)))
            .collect();
        Arc::new(Self {
            table,
            handles: items.iter().map(|&(handle, _)| handle).collect(),
            sources,
            waker,
            indexed_wakers,
            state: MutexCell::new(SignalWaitState {
                handles: 0,
                sources: 0,
                canceled: false,
            }),
        })
    }

    fn arm(self: &Arc<Self>, items: &[(HandleId, u32)]) -> Result<(), IpcError> {
        // Пробуждение scheduler под handle-table локом запрещено.
        let cancel: Arc<dyn CancelTarget> = self.clone();
        self.state.with_lock(|state| {
            if state.canceled {
                return Err(IpcError::Canceled);
            }
            self.table.with_lock(|tbl| {
                for (registered, &handle) in self.handles.iter().enumerate() {
                    // Handle прошёл снимок источников, а к взведению исчез:
                    // закрыт или передан параллельно, и исход ожидания - `Canceled`.
                    if tbl.register_cancel(handle, cancel.clone()).is_err() {
                        for &done in self.handles.iter().take(registered) {
                            tbl.unregister_cancel(done, cancel.as_ref());
                        }
                        return Err(IpcError::Canceled);
                    }
                }
                Ok(())
            })?;
            state.handles = self.handles.len();
            for ((source, indexed), &(_handle, mask)) in self
                .sources
                .iter()
                .zip(self.indexed_wakers.iter())
                .zip(items.iter())
            {
                let dyn_waker: Arc<dyn Waker> = indexed.clone();
                source.register_waiter(mask, dyn_waker);
                state.sources += 1;
            }
            Ok(())
        })
    }

    fn detach(&self) {
        self.state.with_lock(|state| {
            for (source, indexed) in self
                .sources
                .iter()
                .zip(self.indexed_wakers.iter())
                .take(state.sources)
            {
                let dyn_waker: Arc<dyn Waker> = indexed.clone();
                source.remove_waiter(&dyn_waker);
            }
            state.sources = 0;
            self.table.with_lock(|tbl| {
                for &handle in self.handles.iter().take(state.handles) {
                    tbl.unregister_cancel(handle, self);
                }
            });
            state.handles = 0;
        });
    }
}

impl CancelTarget for SignalWaitCancel {
    fn cancel(&self) {
        self.state.with_lock(|state| state.canceled = true);
        self.detach();
        CancelTarget::cancel(self.waker.as_ref());
    }
}

struct ArmedWait {
    cancel: Arc<SignalWaitCancel>,
}

impl ArmedWait {
    fn new(
        table: Arc<MutexCell<HandleTable>>,
        items: &[(HandleId, u32)],
        sources: Vec<Arc<dyn Waitable>>,
        waker: Arc<ParkWaker>,
    ) -> Self {
        Self {
            cancel: SignalWaitCancel::new(table, items, sources, waker),
        }
    }

    fn arm(&self, items: &[(HandleId, u32)]) -> Result<(), IpcError> {
        self.cancel.arm(items)
    }

    fn waker(&self) -> &Arc<ParkWaker> {
        &self.cancel.waker
    }

    fn cancel_target(&self) -> Arc<dyn CancelTarget> {
        self.cancel.clone()
    }
}

impl Drop for ArmedWait {
    fn drop(&mut self) {
        self.cancel.detach();
    }
}

/// Ждёт пока хотя бы на одном из `items` не поднимется бит из его
/// маски. Дубликаты `HandleId` допустимы. Семантика `timeout_ns` и требования к хендлам - как у
/// [`signal_wait_one`]. `Canceled` - один из handle закрыт или передан до сигнала.
///
/// Пустой `items` отвергается с `BadHandle`, набор длиннее `u32::MAX` - с `MessageTooBig`.
/// Опрос до парковки идёт по порядку `items`, и среди готовых сразу побеждает первый по счёту.
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

    let deadline_ns = deadline_after(runtime, timeout_ns);
    let waker = Arc::new(ParkWaker::new(
        runtime.clone(),
        runtime.current_wait_token(),
    ));
    let armed = ArmedWait::new(table, items, sources, waker);
    runtime.set_blocked_cancel(armed.cancel_target());
    if let Err(error) = armed.arm(items) {
        armed.cancel_target().cancel();
        // Для живого current CANCELED-флаг делает checkpoint no-op;
        // Terminated current обязан уйти с CPU до возврата из syscall.
        runtime.block_current_until(armed.waker().state(), None);
        drop(armed);
        runtime.clear_blocked_cancel();
        return Err(error);
    }

    park_until_deadline(runtime, armed.waker(), deadline_ns);
    let resolution = armed.waker().resolve();
    let winner_index = armed.waker().winner_index();
    let observed = armed.waker().observed();
    drop(armed);
    runtime.clear_blocked_cancel();

    match resolution {
        ParkResolution::Signaled => Ok(WaitManyOutcome {
            index: winner_index,
            observed,
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
///
/// Закрытие отзывает поддерево деривации капы. Производные от неё хендлы в чужих таблицах
/// перестают быть валидными и отвечают [`IpcError::Revoked`]. Ожидания, зарегистрированные на
/// этом слоте, снимаются с `Canceled`. Слот уходит в свободный список и при следующей выдаче
/// получает новое поколение.
pub fn handle_close(handle_id: HandleId) -> Result<(), IpcError> {
    current_table()?
        .with_lock(|tbl| tbl.remove(handle_id))?
        .release();
    Ok(())
}

/// Создаёт новый handle на тот же capability target с подмножеством прав и (опционально)
/// значком (badge). Требует [`Rights::DUPLICATE`] на исходном handle.
/// Семантика значка - set-once: заклеймить можно только незаклеймённый хендл. Заклеймённый
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

/// Снимает текущий поток с CPU, когда ядро пометило этот поток завершённым.
/// Возвращается, когда текущий поток жив либо рантайм не установлен.
pub fn exit_if_terminated() {
    if let Some(runtime) = installed_runtime() {
        runtime.exit_if_terminated();
    }
}

/// Завершает все потоки текущего процесса с заданным `exit_code`, поднимает
/// `SIGNALED` на их lifecycle-object и lifecycle-object процесса, освобождает
/// ресурсы процесса и переключает контекст на следующий runnable. Не возвращается.
pub fn process_exit(exit_code: i32) -> ! {
    runtime().exit_current_process(exit_code)
}

/// Уступает остаток кванта текущего потока. Поток переходит в конец своей
/// ready-очереди и остаётся runnable.
pub fn thread_yield() {
    runtime().yield_current();
}

/// Имя текущего процесса для диагностики. `None` до установки рантайма и
/// когда рантайм имён не ведёт.
pub fn current_process_name() -> Option<StaticString<MAX_PROCESS_NAME_LEN>> {
    installed_runtime()?.current_process_name()
}

/// Создаёт новый [`Signal`] и регистрирует handle в таблице текущего
/// процесса. Стартовые права - [`default_rights_for`](super::rights::default_rights_for).
pub fn signal_create() -> Result<HandleId, IpcError> {
    install_handle(Capability::new_with_default_rights(Signal::new()))
}

// Завершение Process/Thread наблюдается ожиданием прямо по их handle через
// `CapabilityTarget::as_waitable` (bound-Signal терминации материализуется
// лениво) - отдельная op материализации Signal-хендла больше не нужна.
// Подделать событие нельзя: `signal_set` строго принимает только `Signal`.

/// Минтит [`IrqLine`] для линии `irq` с триггером `trigger` по полномочию
/// `control_handle`.
/// Требует [`Rights::WRITE`] на `IrqControl`-хендле и попадания `irq` в его
/// диапазон (`permits`).
/// Линия привязывается к контроллеру, который держит полномочие. Возвращает
/// handle на свежий `IrqLine` со стартовыми правами
/// [`default_rights_for`](super::rights::default_rights_for).
///
/// Разделяемых линий в проекте не существует, и минт уже привязанной линии отвергается с
/// [`IpcError::AlreadyBound`]. Отзыв выданной капы отвязывает линию.
pub fn irq_mint(
    control_handle: HandleId,
    irq: u16,
    trigger: IrqTrigger,
) -> Result<HandleId, IpcError> {
    let table = current_table()?;
    let control =
        table.with_lock(|tbl| tbl.get_object::<IrqControl>(control_handle, Rights::WRITE))?;
    if !control.permits(irq) {
        return Err(IpcError::AccessDenied);
    }

    let line = IrqLine::bind(control.interrupts().clone(), irq, trigger)?;
    let hook: Weak<dyn RevocationHook> = Arc::<IrqLine>::downgrade(&line);
    let handle = Capability::new_with_default_rights(line);
    // Хук регистрируется до вставки корня в таблицу, пока корень недоступен другим потокам.
    handle.node().register_hook(hook);
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

    use kbase::MutexCell;

    use super::*;
    use crate::{
        HandleTable, ProcessObject, Rights, Signal, ThreadObject,
        handle::Capability,
        runtime::{KernelRuntime, WaitToken, install_runtime},
        signal::SIGNALED,
        target::CapabilityTarget,
    };

    const PANIC_SENTINEL: &str = "exit-mock-noreturn";
    static CAPTURED_EXIT_CODE: AtomicI32 = AtomicI32::new(i32::MIN);
    static CAPTURED_PROCESS_EXIT_CODE: AtomicI32 = AtomicI32::new(i32::MIN);

    type BlockHook = Box<dyn FnMut() + Send>;

    struct MockRuntime {
        handle_table: Mutex<Option<Arc<MutexCell<HandleTable>>>>,
        block_hook: Mutex<Option<BlockHook>>,
        blocked_cancel: Mutex<Option<Arc<dyn CancelTarget>>>,
        orphaned: Mutex<Vec<Arc<ProcessObject>>>,
    }

    impl MockRuntime {
        fn new() -> Self {
            Self {
                handle_table: Mutex::new(None),
                block_hook: Mutex::new(None),
                blocked_cancel: Mutex::new(None),
                orphaned: Mutex::new(Vec::new()),
            }
        }

        fn take_orphaned(&self) -> Vec<Arc<ProcessObject>> {
            core::mem::take(&mut *self.orphaned.lock().unwrap())
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
            *self.blocked_cancel.lock().unwrap() = None;
            self.orphaned.lock().unwrap().clear();
        }

        fn cancel_blocked(&self) {
            let cancel = self
                .blocked_cancel
                .lock()
                .unwrap()
                .clone()
                .expect("blocked cancel is armed");
            cancel.cancel();
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

        fn exit_current_process(&self, exit_code: i32) -> ! {
            CAPTURED_PROCESS_EXIT_CODE.store(exit_code, Ordering::SeqCst);
            panic!("{PANIC_SENTINEL}");
        }

        fn yield_current(&self) {}

        fn block_current_until(&self, _ready_flag: &AtomicU32, _timeout_ns: Option<u64>) {
            if let Some(hook) = self.block_hook.lock().unwrap().as_mut() {
                hook();
            }
        }

        fn unblock(&self, _token: WaitToken) {}

        fn set_blocked_cancel(&self, cancel: Arc<dyn crate::CancelTarget>) {
            *self.blocked_cancel.lock().unwrap() = Some(cancel);
        }

        fn clear_blocked_cancel(&self) {
            *self.blocked_cancel.lock().unwrap() = None;
        }

        fn release_process_holder(&self, process: &Arc<ProcessObject>) {
            self.orphaned.lock().unwrap().push(process.clone());
        }
    }

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
    fn last_process_holder_leaving_reports_to_runtime() {
        let _guard = test_lock();
        mock().reset();

        let process = ProcessObject::new();
        let first = Capability::new(
            CapabilityTarget::Process(process.clone()),
            Rights::READ | Rights::WRITE | Rights::DUPLICATE,
        );
        let second = first.duplicate(Rights::READ | Rights::WRITE, 0).unwrap();
        assert_eq!(process.holder_count(), 2);

        drop(first);
        assert!(mock().take_orphaned().is_empty());

        drop(second);
        let orphaned = mock().take_orphaned();
        assert_eq!(orphaned.len(), 1);
        assert!(Arc::ptr_eq(&orphaned[0], &process));
    }

    #[test]
    fn observer_without_write_does_not_hold_the_process() {
        let _guard = test_lock();
        mock().reset();

        let process = ProcessObject::new();
        let observer = Capability::new(CapabilityTarget::Process(process.clone()), Rights::READ);
        assert_eq!(process.holder_count(), 0);

        drop(observer);
        assert!(mock().take_orphaned().is_empty());
    }

    #[test]
    fn unheld_capability_leaves_the_process_untouched() {
        let _guard = test_lock();
        mock().reset();

        let process = ProcessObject::new();
        let holder = Capability::new(
            CapabilityTarget::Process(process.clone()),
            Rights::READ | Rights::WRITE,
        );
        let unheld = Capability::new_unheld_process(process.clone(), Rights::READ | Rights::WRITE);
        assert_eq!(process.holder_count(), 1);

        drop(unheld);
        assert_eq!(process.holder_count(), 1);
        assert!(mock().take_orphaned().is_empty());

        drop(holder);
        assert_eq!(mock().take_orphaned().len(), 1);
    }

    #[test]
    fn duplicate_of_an_unheld_capability_stays_unheld() {
        let _guard = test_lock();
        mock().reset();

        let process = ProcessObject::new();
        let unheld = Capability::new_unheld_process(
            process.clone(),
            Rights::READ | Rights::WRITE | Rights::DUPLICATE,
        );

        let copy = unheld.duplicate(Rights::READ | Rights::WRITE, 0).unwrap();
        assert_eq!(process.holder_count(), 0);

        drop(copy);
        drop(unheld);
        assert!(mock().take_orphaned().is_empty());
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
    fn process_exit_forwards_code_to_runtime() {
        let _guard = test_lock();
        mock().reset();

        let prev_hook = panic::take_hook();
        panic::set_hook(Box::new(|_| {}));
        let result = panic::catch_unwind(|| process_exit(11));
        panic::set_hook(prev_hook);

        assert!(result.is_err());
        assert_eq!(CAPTURED_PROCESS_EXIT_CODE.load(Ordering::SeqCst), 11);
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

    /// Бессрочное ожидание сигнала не ограничено тайм-аутом. Пробуждение, не принёсшее
    /// сигнала, возвращает поток в ожидание.
    #[test]
    fn signal_wait_without_deadline_waits_out_a_wake_without_a_signal() {
        let _guard = test_lock();
        let (table, id, signal) = install_signal_handle(Rights::READ | Rights::WRITE);
        let parks = Arc::new(AtomicU32::new(0));
        let (signal_for_hook, parks_for_hook) = (signal.clone(), parks.clone());
        let hook: BlockHook = Box::new(move || {
            // Первое пробуждение сигнала не приносит.
            if parks_for_hook.fetch_add(1, Ordering::AcqRel) == 1 {
                signal_for_hook.signal(SIGNALED, 0);
            }
        });
        mock().configure(Some(table), Some(hook));

        let observed = signal_wait_one(id, SIGNALED, None).expect("Ok");
        assert_eq!(observed & SIGNALED, SIGNALED);
        assert_eq!(parks.load(Ordering::Acquire), 2);

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

    #[test]
    fn signal_wait_runtime_cancel_detaches_owned_wait() {
        let _guard = test_lock();
        let (table, id, signal) = install_signal_handle(Rights::READ);
        let hook: BlockHook = Box::new(|| mock().cancel_blocked());
        mock().configure(Some(table.clone()), Some(hook));

        let error = signal_wait_one(id, SIGNALED, None).unwrap_err();
        assert_eq!(error, IpcError::Canceled);
        assert_eq!(signal.waiter_count(), 0);

        table.with_lock(|tbl| tbl.remove(id).unwrap()).release();
        signal.signal(SIGNALED, 0);

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
        let armed = ArmedWait::new(table, &items, sources, waker);
        let result = armed.arm(&items);

        assert!(matches!(result, Err(IpcError::Canceled)));

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
        // Право WRITE на Process-хендле не даёт поднять SIGNALED на bound-Signal
        // терминации. Функция signal_set принимает только Signal, а Signal-таргет
        // терминации не выдаётся наружу.
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

    fn irq_control_handle(
        rights: Rights,
        lo: u16,
        hi: u16,
    ) -> (Arc<MutexCell<HandleTable>>, HandleId) {
        let table = Arc::new(MutexCell::new(HandleTable::new()));
        let id = table
            .with_lock(|tbl| {
                tbl.insert(Capability::new(
                    CapabilityTarget::IrqControl(crate::IrqControl::new(
                        Arc::new(crate::irq_runtime::StubInterruptsControl),
                        lo,
                        hi,
                    )),
                    rights,
                ))
            })
            .unwrap();
        (table, id)
    }

    #[test]
    fn irq_mint_returns_irq_line_handle() {
        let _guard = test_lock();
        let (table, ctl_id) = irq_control_handle(Rights::WRITE, 32, 64);
        mock().configure(Some(table.clone()), None);

        let line_id = irq_mint(ctl_id, 40, IrqTrigger::Level).expect("mint ok");
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
        let (table, ctl_id) = irq_control_handle(Rights::WRITE, 32, 64);
        mock().configure(Some(table), None);

        assert_eq!(
            irq_mint(ctl_id, 100, IrqTrigger::Level),
            Err(IpcError::AccessDenied)
        );

        mock().reset();
    }

    #[test]
    fn irq_mint_without_write_is_access_denied() {
        let _guard = test_lock();
        let (table, ctl_id) = irq_control_handle(Rights::READ, 32, 64);
        mock().configure(Some(table), None);

        assert_eq!(
            irq_mint(ctl_id, 40, IrqTrigger::Level),
            Err(IpcError::AccessDenied)
        );

        mock().reset();
    }

    #[test]
    fn irq_mint_on_non_control_is_wrong_type() {
        let _guard = test_lock();
        let (table, id, _sig) = install_signal_handle(Rights::WRITE);
        mock().configure(Some(table), None);

        assert_eq!(
            irq_mint(id, 40, IrqTrigger::Level),
            Err(IpcError::WrongType)
        );

        mock().reset();
    }

    #[test]
    fn irq_ack_on_armed_line_is_wrong_type() {
        let _guard = test_lock();
        let (table, ctl_id) = irq_control_handle(Rights::WRITE, 32, 64);
        mock().configure(Some(table), None);

        let line_id = irq_mint(ctl_id, 40, IrqTrigger::Level).expect("mint ok");
        // Свежеминченная линия в ARMED, подтверждать нечего.
        assert_eq!(irq_ack(line_id), Err(IpcError::WrongType));

        mock().reset();
    }
}
