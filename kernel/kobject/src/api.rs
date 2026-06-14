use alloc::{sync::Arc, vec::Vec};

use collections::LockCell;

use super::{
    channel::{Channel, Message},
    errors::{IpcError, SpawnError},
    event::Event,
    handle::{Handle, HandleId},
    koid::Koid,
    mailbox::{AsyncMode, MAILBOX_READABLE, Mailbox, MailboxPacket},
    object::KObject,
    process::ProcessObject,
    rights::Rights,
    runtime::{ParkState, UserThreadEntry, runtime},
    spawn::{LoadImageError, StartProcessError, UserImageInstall, UserStartSpec},
    thread::ThreadObject,
    wait::{CancelTarget, IndexedWaker, ParkWaker, SignalSource, SignalState, Waker},
};

/// Устанавливает [`Handle`] в handle-таблицу текущего процесса и
/// возвращает свежий [`HandleId`]
pub fn install_handle(handle: Handle) -> Result<HandleId, IpcError> {
    let table = runtime()
        .current_handle_table()
        .ok_or(IpcError::BadHandle)?;
    table.with_lock(|tbl| tbl.insert(handle))
}

/// Атомарно меняет биты сигнального состояния KO (поднимает `set`,
/// снимает `clear`) и будит waiter'ов.
/// При `count == 0` будит всех пересекающихся;
/// При `count == N` - не более N в FIFO-порядке регистрации.
/// Требует [`Rights::SIGNAL`] на handle. Без signals у KO - `WrongType`.
pub fn object_signal(
    handle_id: HandleId,
    set: u32,
    clear: u32,
    count: u32,
) -> Result<(), IpcError> {
    let runtime = runtime();
    let table = runtime.current_handle_table().ok_or(IpcError::BadHandle)?;
    let object = table.with_lock(|tbl| tbl.clone_object(handle_id, Rights::SIGNAL))?;
    let signal_state = object.signals().ok_or(IpcError::WrongType)?;

    let limit = if count == 0 {
        usize::MAX
    } else {
        count as usize
    };

    signal_state.signal_n(set, clear, limit);

    Ok(())
}

/// Блокирует поток до появления хотя бы одного бита из `mask`.
/// Возвращает observed-маску. `timeout_ns = Some(0)` - non-blocking
/// poll. Без cancel-target'а: используется только `mailbox_wait`,
/// чьё ожидание привязано к жизни Mailbox-а.
fn wait_until(
    signal_state: &SignalState,
    mask: u32,
    timeout_ns: Option<u64>,
) -> Result<u32, IpcError> {
    let already = signal_state.peek() & mask;
    if already != 0 {
        return Ok(already);
    }
    if timeout_ns == Some(0) {
        return Err(IpcError::Timeout);
    }

    let runtime = runtime();
    let token = runtime.current_wait_token();
    let waker = Arc::new(ParkWaker::new(runtime.clone(), token));
    let waker_dyn: Arc<dyn Waker> = waker.clone();

    signal_state.register_waiter(mask, waker_dyn.clone());
    runtime.block_current_until(waker.state(), timeout_ns);

    let result = match resolve_wait(&waker) {
        WaitResolution::Signaled => Ok(waker.observed() & mask),
        WaitResolution::Timeout => Err(IpcError::Timeout),
        WaitResolution::Canceled => unreachable!("wait_until does not register cancel target"),
    };
    signal_state.remove_waiter(&waker_dyn);
    result
}

/// Ждёт пока на KO, к которому относится `handle_id`, не поднимется
/// хотя бы один бит из `signals`. На `timeout_ns = Some(0)` - poll.
/// `Canceled` - handle закрыт или передан до сигнала.
pub fn object_wait_one(
    handle_id: HandleId,
    signals: u32,
    timeout_ns: Option<u64>,
) -> Result<u32, IpcError> {
    let outcome = object_wait_many(&[(handle_id, signals)], timeout_ns)?;
    Ok(outcome.observed)
}

/// Исход [`object_wait_many`]: индекс сработавшего item'а в `items`
/// и observed-маска на нём (пересечение с переданной маской).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitManyOutcome {
    pub index: usize,
    pub observed: u32,
}

enum WaitSetup {
    Observed(WaitManyOutcome),
    Wait {
        objects: Vec<KObject>,
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
pub fn object_wait_many(
    items: &[(HandleId, u32)],
    timeout_ns: Option<u64>,
) -> Result<WaitManyOutcome, IpcError> {
    if items.is_empty() {
        return Err(IpcError::BadHandle);
    }

    let runtime = runtime();
    let table = runtime.current_handle_table().ok_or(IpcError::BadHandle)?;

    // Setup под табличным локом: handle_close, racing с регистрацией,
    // либо отработает до нас (и мы вернём BadHandle), либо после
    // (и сработает cancel).
    let (objects, waker, indexed_wakers) =
        match table.with_lock(|tbl| wait_many_setup(tbl, runtime, items, timeout_ns))? {
            WaitSetup::Observed(out) => return Ok(out),
            WaitSetup::Wait {
                objects,
                waker,
                indexed_wakers,
            } => (objects, waker, indexed_wakers),
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

    // Cleanup идемпотентен: запись могла быть уже удалена signal/cancel-стороной.
    for (obj, indexed) in objects.iter().zip(indexed_wakers.iter()) {
        if let Some(ss) = obj.signals() {
            let dyn_waker: Arc<dyn Waker> = indexed.clone();
            ss.remove_waiter(&dyn_waker);
        }
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
    let mut objects: Vec<KObject> = Vec::with_capacity(items.len());
    for &(h, _mask) in items {
        let obj = tbl.clone_object(h, Rights::WAIT)?;
        obj.signals().ok_or(IpcError::WrongType)?;
        objects.push(obj);
    }

    for (i, (obj, &(_h, mask))) in objects.iter().zip(items.iter()).enumerate() {
        let already = obj.signals().expect("validated above").peek() & mask;
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

    for (i, (obj, &(h, mask))) in objects.iter().zip(items.iter()).enumerate() {
        let index = u32::try_from(i).expect("count ≤ u32::MAX by ABI");
        let indexed = Arc::new(IndexedWaker::new(waker.clone(), index));
        let dyn_waker: Arc<dyn Waker> = indexed.clone();
        obj.signals()
            .expect("validated above")
            .register_waiter(mask, dyn_waker);
        indexed_wakers.push(indexed);
        let cancel: Arc<dyn CancelTarget> = waker.clone();
        tbl.register_cancel(h, cancel)
            .expect("slot validated above");
    }

    Ok(WaitSetup::Wait {
        objects,
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

/// Создаёт пару связанных `Channel` и регистрирует оба handle'а
/// в handle-table текущего процесса. Возвращает пару идентификаторов
/// `(left, right)` - endpoint'ы симметричны, любую сторону можно
/// использовать как "свою" и передавать парную через
/// [`Message::push_handle`]. Стартовые права берутся из
/// [`Rights::defaults_for`].
///
/// При неудаче регистрации второго handle'а первый автоматически
/// снимается из таблицы (без утечки слота).
pub fn channel_create() -> Result<(HandleId, HandleId), IpcError> {
    let table = runtime()
        .current_handle_table()
        .ok_or(IpcError::BadHandle)?;

    let (left_endpoint, right_endpoint) = Channel::create_pair(0);

    let left_ko = KObject::Channel(left_endpoint);
    let right_ko = KObject::Channel(right_endpoint);
    let left_handle = Handle::new(left_ko.clone(), Rights::defaults_for(&left_ko));
    let right_handle = Handle::new(right_ko.clone(), Rights::defaults_for(&right_ko));

    table.with_lock(|tbl| {
        let left_id = tbl.insert(left_handle)?;
        match tbl.insert(right_handle) {
            Ok(right_id) => Ok((left_id, right_id)),
            Err(e) => {
                // Снимаем уже зарегистрированный левый endpoint, чтобы
                // не оставлять в таблице "висящий" handle.
                let _ = tbl.remove(left_id);
                Err(e)
            }
        }
    })
}

/// Помещает сообщение в парный endpoint канала, на который указывает
/// `handle_id`. Требует [`Rights::WRITE`]; на не-channel handle -
/// `WrongType`.
pub fn channel_write(handle_id: HandleId, msg: Message) -> Result<(), IpcError> {
    let table = runtime()
        .current_handle_table()
        .ok_or(IpcError::BadHandle)?;
    let endpoint = table.with_lock(|tbl| tbl.get_channel(handle_id, Rights::WRITE))?;
    endpoint.write(msg)
}

/// Достаёт сообщение из inbound-очереди endpoint'а, на который
/// указывает `handle_id`. Требует [`Rights::READ`]; на не-channel
/// handle - `WrongType`. Если очередь пуста - `ShouldWait` (или
/// `PeerClosed`, если парный endpoint закрыт).
pub fn channel_read(handle_id: HandleId) -> Result<Message, IpcError> {
    let table = runtime()
        .current_handle_table()
        .ok_or(IpcError::BadHandle)?;
    let endpoint = table.with_lock(|tbl| tbl.get_channel(handle_id, Rights::READ))?;
    endpoint.read()
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

/// Создаёт новый handle на тот же KO с подмножеством прав. Требует
/// [`Rights::DUPLICATE`] на исходном handle и `new_rights ⊆ rights`
/// (иначе - `AccessDenied`).
pub fn handle_duplicate(handle_id: HandleId, new_rights: Rights) -> Result<HandleId, IpcError> {
    let table = runtime()
        .current_handle_table()
        .ok_or(IpcError::BadHandle)?;
    table.with_lock(|tbl| tbl.duplicate(handle_id, new_rights))
}

/// Завершает текущий поток с заданным `exit_code`. Поднимает
/// `THREAD_TERMINATED` на `Arc<ThreadObject>` уходящего потока (а на
/// последнем потоке процесса - `PROCESS_TERMINATED`) и переключает
/// контекст на следующий runnable. Не возвращается.
pub fn thread_exit(exit_code: i32) -> ! {
    runtime().exit_current_thread(exit_code)
}

/// Создаёт пустой user-процесс через [`KernelRuntime::create_empty_process`].
pub fn create_empty_process(name: &str) -> Result<Arc<ProcessObject>, SpawnError> {
    runtime().create_empty_process(name)
}

/// Создаёт user-поток в указанном процессе и помещает его в ready-queue.
pub fn create_user_thread(
    process: &Arc<ProcessObject>,
    entry: UserThreadEntry,
) -> Result<Arc<ThreadObject>, SpawnError> {
    runtime().create_user_thread(process, entry)
}

/// Устанавливает регионы образа в child AS и прикрепляет user_vm-аллокатор.
pub fn load_user_image_into(
    process: &Arc<ProcessObject>,
    install: &UserImageInstall,
) -> Result<(), LoadImageError> {
    runtime().load_user_image_into(process, install)
}

/// Вставляет bootstrap-handles в child handle-таблицу и стартует первый
/// user-поток.
pub fn start_user_process(
    process: &Arc<ProcessObject>,
    spec: UserStartSpec,
) -> Result<Arc<ThreadObject>, StartProcessError> {
    runtime().start_user_process(process, spec)
}

/// Идемпотентно завершает поток: поднимает `THREAD_TERMINATED`,
/// декрементирует thread_count процесса; на нуле - поднимает
/// `PROCESS_TERMINATED`.
pub fn terminate_thread(thread: &Arc<ThreadObject>, exit_code: i32) -> Result<(), IpcError> {
    runtime().terminate_thread(thread, exit_code)
}

/// Идемпотентно завершает процесс: всем его живым потокам поднимает
/// `THREAD_TERMINATED`, после декремента до нуля - `PROCESS_TERMINATED`.
pub fn terminate_process(process: &Arc<ProcessObject>, exit_code: i32) -> Result<(), IpcError> {
    runtime().terminate_process(process, exit_code)
}

/// Создаёт новый [`Event`] и регистрирует handle в таблице текущего процесса.
/// Стартовые права - [`Rights::defaults_for`].
pub fn event_create() -> Result<HandleId, IpcError> {
    let table = runtime()
        .current_handle_table()
        .ok_or(IpcError::BadHandle)?;
    let event = Event::new();
    let ko = KObject::Event(event);
    let handle = Handle::new(ko.clone(), Rights::defaults_for(&ko));
    table.with_lock(|tbl| tbl.insert(handle))
}

/// Создаёт пустой [`Mailbox`] и регистрирует handle в таблице
/// текущего процесса. Стартовые права - [`Rights::defaults_for`].
pub fn mailbox_create() -> Result<HandleId, IpcError> {
    let table = runtime()
        .current_handle_table()
        .ok_or(IpcError::BadHandle)?;
    let mb = Mailbox::new();
    let ko = KObject::Mailbox(mb);
    let handle = Handle::new(ko.clone(), Rights::defaults_for(&ko));
    table.with_lock(|tbl| tbl.insert(handle))
}

/// Кладёт пакет в mailbox. Требует [`Rights::WRITE`]; на полной
/// очереди - [`IpcError::ShouldWait`] без побочных эффектов.
pub fn mailbox_queue(handle_id: HandleId, packet: MailboxPacket) -> Result<(), IpcError> {
    let table = runtime()
        .current_handle_table()
        .ok_or(IpcError::BadHandle)?;
    let mb = table.with_lock(|tbl| tbl.get_mailbox(handle_id, Rights::WRITE))?;
    mb.queue(packet)
}

/// Извлекает `Arc<dyn SignalSource>` и `Koid` для KO, на который указывает
/// `handle_id`, проверив `Rights::WAIT`. Memory/PhysicalResource и
/// Mailbox - `WrongType`.
///
/// Mailbox в качестве target отвергается на api-границе: `Mailbox::queue`
/// зовёт `signal()` под mailbox.inner-локом, observer-wake берёт
/// target-mailbox.inner. Цепочка `A -> B -> A` (или `A -> A` через
/// два handle одного KO) при `queue` возвращается на уже удерживаемый
/// `inner` - recursive spinlock. Запрет здесь делает все mailbox-цепочки
/// невозможными независимо от того, как у пользователя выглядит handle.
fn target_signal_source(handle_id: HandleId) -> Result<(Arc<dyn SignalSource>, Koid), IpcError> {
    let table = runtime()
        .current_handle_table()
        .ok_or(IpcError::BadHandle)?;
    let object = table.with_lock(|tbl| tbl.clone_object(handle_id, Rights::WAIT))?;
    let koid = object.koid();
    let source: Arc<dyn SignalSource> = match object {
        KObject::Channel(c) => c,
        KObject::Event(e) => e,
        KObject::Process(p) => p,
        KObject::Thread(t) => t,
        KObject::Mailbox(_) | KObject::Memory(_) | KObject::PhysicalResource(_) => {
            return Err(IpcError::WrongType);
        }
    };
    Ok((source, koid))
}

/// Подписывает mailbox на сигналы target'а: при поднятии хотя бы
/// одного бита из `mask` ядро положит в очередь mailbox'а signal-
/// пакет с заданным `key`.
///
/// Требует [`Rights::WRITE`] на mailbox-handle и [`Rights::WAIT`] на
/// target-handle. Backpressure - drop_newest: на полной очереди
/// signal-пакет дропается, [`Mailbox::overflow_count`] инкрементится.
pub fn mailbox_wait_async(
    mailbox: HandleId,
    target: HandleId,
    key: u64,
    mask: u32,
    mode: AsyncMode,
) -> Result<(), IpcError> {
    let table = runtime()
        .current_handle_table()
        .ok_or(IpcError::BadHandle)?;
    let mb = table.with_lock(|tbl| tbl.get_mailbox(mailbox, Rights::WRITE))?;
    let (source, koid) = target_signal_source(target)?;
    mb.subscribe(&source, koid, key, mask, mode)
}

/// Отзывает подписку, ранее зарегистрированную через
/// [`mailbox_wait_async`]. Идемпотентна: если подписка с таким
/// `(target_koid, key)` отсутствует - `Ok(())`. Требует
/// [`Rights::WRITE`] на mailbox-handle.
pub fn mailbox_cancel(mailbox: HandleId, target: HandleId, key: u64) -> Result<(), IpcError> {
    let table = runtime()
        .current_handle_table()
        .ok_or(IpcError::BadHandle)?;
    let mb = table.with_lock(|tbl| tbl.get_mailbox(mailbox, Rights::WRITE))?;
    // Источник нам нужен только для извлечения koid'а: cancel хранит
    // Weak<target> внутри Observer, поэтому повторный target не нужен.
    let (_source, koid) = target_signal_source(target)?;
    mb.cancel_subscription(koid, key);
    Ok(())
}

/// Атомарный wait+pop: блокируется до появления пакета или истечения
/// `timeout_ns`, затем атомарно достаёт первый. Требует
/// [`Rights::READ`].
///
/// На multi-consumer пути возможен ложный возврат из `wait_until`
/// (другой consumer успел опустошить очередь между wake и нашим
/// `try_pop`); в этом случае цикл уходит в следующий wait. С
/// `timeout_ns = Some(0)` лишний цикл невозможен: poll либо
/// возвращает пакет, либо `Timeout`.
pub fn mailbox_wait(
    handle_id: HandleId,
    timeout_ns: Option<u64>,
) -> Result<MailboxPacket, IpcError> {
    let table = runtime()
        .current_handle_table()
        .ok_or(IpcError::BadHandle)?;
    let mb = table.with_lock(|tbl| tbl.get_mailbox(handle_id, Rights::READ))?;

    loop {
        match mb.try_pop() {
            Ok(packet) => return Ok(packet),
            Err(IpcError::ShouldWait) => {
                wait_until(mb.signals(), MAILBOX_READABLE, timeout_ns)?;
            }
            Err(e) => return Err(e),
        }
    }
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
        Event, HandleTable, ProcessObject, Rights, ThreadObject,
        event::EVENT_SIGNALED,
        handle::Handle,
        object::KObject,
        runtime::{KernelRuntime, UserThreadEntry, WaitToken, install_runtime},
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

        fn current_thread_object(&self) -> Option<Arc<ThreadObject>> {
            None
        }

        fn current_process_object(&self) -> Option<Arc<ProcessObject>> {
            None
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

        fn create_empty_process(&self, _name: &str) -> Result<Arc<ProcessObject>, SpawnError> {
            Err(SpawnError::NoFreeProcessSlots)
        }

        fn create_user_thread(
            &self,
            _process: &Arc<ProcessObject>,
            _entry: UserThreadEntry,
        ) -> Result<Arc<ThreadObject>, SpawnError> {
            Err(SpawnError::NoFreeThreadSlots)
        }

        fn terminate_thread(
            &self,
            _thread: &Arc<ThreadObject>,
            _exit_code: i32,
        ) -> Result<(), IpcError> {
            Ok(())
        }

        fn terminate_process(
            &self,
            _process: &Arc<ProcessObject>,
            _exit_code: i32,
        ) -> Result<(), IpcError> {
            Ok(())
        }

        fn load_user_image_into(
            &self,
            _process: &Arc<ProcessObject>,
            _install: &super::UserImageInstall,
        ) -> Result<(), super::LoadImageError> {
            Err(super::LoadImageError::ProcessNotFound)
        }

        fn start_user_process(
            &self,
            _process: &Arc<ProcessObject>,
            _spec: super::UserStartSpec,
        ) -> Result<Arc<ThreadObject>, super::StartProcessError> {
            Err(super::StartProcessError::ProcessNotFound)
        }
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

    fn install_event_handle(rights: Rights) -> (Arc<MutexCell<HandleTable>>, HandleId, Arc<Event>) {
        let table = Arc::new(MutexCell::new(HandleTable::new()));
        let event = Event::new();
        let id = table
            .with_lock(|tbl| tbl.insert(Handle::new(KObject::Event(event.clone()), rights)))
            .unwrap();
        (table, id, event)
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

    /// Regression guard: signal внутри парковки + clear до resolve ->
    /// `Ok(observed)`, а не Timeout. Любая попытка вернуть re-peek
    /// `SignalState` сломает тест.
    #[test]
    fn object_wait_one_returns_latched_observed_after_signal_state_cleared() {
        let _guard = test_lock();
        let (table, id, event) = install_event_handle(Rights::WAIT | Rights::SIGNAL);
        let event_for_hook = event.clone();
        let hook: BlockHook = Box::new(move || {
            event_for_hook.signal(EVENT_SIGNALED, 0);
            event_for_hook.signal(0, EVENT_SIGNALED);
        });
        mock().configure(Some(table), Some(hook));

        let observed = object_wait_one(id, EVENT_SIGNALED, None).expect("Ok");
        assert_eq!(observed & EVENT_SIGNALED, EVENT_SIGNALED);
        assert_eq!(event.peek() & EVENT_SIGNALED, 0);

        mock().reset();
    }

    #[test]
    fn object_wait_many_returns_latched_outcome_for_winning_item() {
        let _guard = test_lock();
        let (table, id_a, _ev_a) = install_event_handle(Rights::WAIT | Rights::SIGNAL);
        let event_b = Event::new();
        let id_b = table
            .with_lock(|tbl| {
                tbl.insert(Handle::new(
                    KObject::Event(event_b.clone()),
                    Rights::WAIT | Rights::SIGNAL,
                ))
            })
            .unwrap();

        let event_b_for_hook = event_b.clone();
        let hook: BlockHook = Box::new(move || {
            event_b_for_hook.signal(EVENT_SIGNALED, 0);
            event_b_for_hook.signal(0, EVENT_SIGNALED);
        });
        mock().configure(Some(table), Some(hook));

        let outcome =
            object_wait_many(&[(id_a, EVENT_SIGNALED), (id_b, EVENT_SIGNALED)], None).expect("Ok");
        assert_eq!(outcome.index, 1);
        assert_eq!(outcome.observed & EVENT_SIGNALED, EVENT_SIGNALED);
        assert_eq!(event_b.peek() & EVENT_SIGNALED, 0);

        mock().reset();
    }

    #[test]
    fn object_wait_one_returns_canceled_when_handle_closed_during_wait() {
        let _guard = test_lock();
        let (table, id, _event) = install_event_handle(Rights::WAIT);
        let table_for_hook = table.clone();
        let hook: BlockHook = Box::new(move || {
            table_for_hook.with_lock(|tbl| tbl.remove(id).unwrap());
        });
        mock().configure(Some(table), Some(hook));

        let err = object_wait_one(id, EVENT_SIGNALED, None).unwrap_err();
        assert_eq!(err, IpcError::Canceled);

        mock().reset();
    }

    #[test]
    fn object_wait_many_canceled_when_any_handle_closed_during_wait() {
        let _guard = test_lock();
        let (table, id, _event) = install_event_handle(Rights::WAIT);
        let table_for_hook = table.clone();
        let hook: BlockHook = Box::new(move || {
            table_for_hook.with_lock(|tbl| tbl.remove(id).unwrap());
        });
        mock().configure(Some(table), Some(hook));

        let err =
            object_wait_many(&[(id, EVENT_SIGNALED), (id, EVENT_SIGNALED)], None).unwrap_err();
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
    fn object_signal_with_count_limits_wakeups() {
        let _guard = test_lock();
        let (table, id, event) = install_event_handle(Rights::SIGNAL);
        mock().configure(Some(table), None);

        let w1 = CountingWaker::new();
        let w2 = CountingWaker::new();
        event.signals().register_waiter(EVENT_SIGNALED, w1.clone());
        event.signals().register_waiter(EVENT_SIGNALED, w2.clone());

        object_signal(id, EVENT_SIGNALED, 0, 1).expect("signal ok");
        assert!(w1.was_woken());
        assert!(!w2.was_woken());

        mock().reset();
    }

    #[test]
    fn object_signal_with_zero_count_wakes_all() {
        let _guard = test_lock();
        let (table, id, event) = install_event_handle(Rights::SIGNAL);
        mock().configure(Some(table), None);

        let w1 = CountingWaker::new();
        let w2 = CountingWaker::new();
        event.signals().register_waiter(EVENT_SIGNALED, w1.clone());
        event.signals().register_waiter(EVENT_SIGNALED, w2.clone());

        object_signal(id, EVENT_SIGNALED, 0, 0).expect("signal ok");
        assert!(w1.was_woken());
        assert!(w2.was_woken());

        mock().reset();
    }

    #[test]
    fn event_create_inserts_handle_with_signal_and_wait_rights() {
        let _guard = test_lock();
        let table = Arc::new(MutexCell::new(HandleTable::new()));
        mock().configure(Some(table.clone()), None);

        let id = event_create().expect("event_create ok");
        let rights = table
            .with_lock(|tbl| {
                tbl.get(id, Rights::SIGNAL | Rights::WAIT)
                    .map(Handle::rights)
            })
            .expect("handle present with SIGNAL|WAIT");
        assert!(rights.contains(Rights::SIGNAL));
        assert!(rights.contains(Rights::WAIT));

        mock().reset();
    }
}
