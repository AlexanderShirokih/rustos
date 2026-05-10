//! Имитирует будущий syscall-слой: на вход - `HandleId`, на выход -
//! `Result<_, IpcError>`. Внутри идёт через [`runtime()`](super::runtime)
//! к per-process `HandleTable` и scheduler-у. Никакой `KObject` наружу
//! не утекает.

use alloc::sync::Arc;

use collections::LockCell;

use super::{
    channel::{Channel, Message},
    errors::{IpcError, SpawnError},
    handle::{Handle, HandleId},
    koid::Koid,
    mailbox::{AsyncMode, MAILBOX_READABLE, Mailbox, MailboxPacket},
    object::KObject,
    process::ProcessObject,
    rights::Rights,
    runtime::{ParkState, UserThreadEntry, runtime},
    thread::ThreadObject,
    wait::{ParkWaker, SignalSource, SignalState, Waker},
};

/// Устанавливает [`Handle`] в handle-table текущего процесса и
/// возвращает свежий [`HandleId`]. Удобный сахар для перехода между
/// "у меня есть `KObject`" и handle-based API.
pub fn install_handle(handle: Handle) -> Result<HandleId, IpcError> {
    let table = runtime()
        .current_handle_table()
        .ok_or(IpcError::BadHandle)?;
    table.with_lock(|tbl| tbl.insert(handle))
}

/// Атомарно меняет биты сигнального состояния KO (поднимает `set`,
/// снимает `clear`). Требует [`Rights::SIGNAL`] на handle. Без
/// signals у KO - `WrongType`.
pub fn object_signal(handle_id: HandleId, set: u32, clear: u32) -> Result<(), IpcError> {
    let runtime = runtime();
    let table = runtime.current_handle_table().ok_or(IpcError::BadHandle)?;
    let object = table.with_lock(|tbl| tbl.clone_object(handle_id, Rights::SIGNAL))?;
    let signal_state = object.signals().ok_or(IpcError::WrongType)?;
    signal_state.signal(set, clear);
    Ok(())
}

/// Блокирует current thread до тех пор, пока в `signal_state` не
/// поднимется хотя бы один бит из `mask`. Возвращает наблюдённую
/// маску сигналов (пересечение `observed & mask`).
///
/// Совместно используется [`object_wait_one`] и [`mailbox_wait`]:
/// первый - для KO-универсального wait, второй - в цикле перед
/// [`Mailbox::try_pop`]. Регистрирует [`ParkWaker`], паркует поток,
/// разрешает гонку signal/timeout через CAS на [`ParkState`].
///
/// `timeout_ns = Some(0)` - non-blocking poll: возвращает уже
/// набранную маску, иначе [`IpcError::Timeout`].
fn wait_until(
    signal_state: &SignalState,
    mask: u32,
    timeout_ns: Option<u64>,
) -> Result<u32, IpcError> {
    // Fast-path: уже сигналит.
    let already = signal_state.peek() & mask;
    if already != 0 {
        return Ok(already);
    }

    // Non-blocking poll: не регистрируем waiter и не паркуем поток -
    // иначе block_current_until уйдёт в context switch на дедлайне now+0.
    if timeout_ns == Some(0) {
        return Err(IpcError::Timeout);
    }

    let runtime = runtime();
    let token = runtime.current_wait_token();
    let waker_arc = Arc::new(ParkWaker::new(runtime.clone(), token));
    let waker_dyn: Arc<dyn Waker> = waker_arc.clone();

    signal_state.register_waiter(mask, waker_dyn.clone());

    // register_waiter под своим локом мог уже вызвать wake() (если
    // signal-биты были выставлены): тогда state == SIGNALED - и
    // block_current_until сразу вернётся, не паркуя поток.
    runtime.block_current_until(waker_arc.state(), timeout_ns);

    // Поток возобновился: либо по signal-стороне (state SIGNALED), либо
    // по timeout-стороне (state ещё REGISTERED - сонник снял с
    // SleepQueue). Закрываем гонку через CAS.
    match waker_arc
        .state()
        .load(core::sync::atomic::Ordering::Acquire)
    {
        ParkState::SIGNALED => Ok(waker_arc.observed() & mask),
        _ => {
            if waker_arc.claim_timeout() {
                // Снимаем waker'а из списка, чтобы поздний signal не
                // зацепил уже отпущенный поток.
                signal_state.remove_waiter(&waker_dyn);
                Err(IpcError::Timeout)
            } else {
                // Signal победил между нашей загрузкой state и CAS -
                // используем его наблюдение.
                Ok(waker_arc.observed() & mask)
            }
        }
    }
}

/// Ждёт пока на KO, к которому относится `handle_id`, не поднимется
/// хотя бы один бит из `signals`. Возвращает наблюдённую маску
/// сигналов. На `timeout_ns = Some(0)` - полу-non-blocking poll.
///
/// - `BadHandle` / `AccessDenied`: проблемы с handle (право `WAIT`).
/// - `WrongType`: KO не сигнализуем (например, Process в текущей фазе).
/// - `Timeout`: истёк дедлайн до сигнала.
pub fn object_wait_one(
    handle_id: HandleId,
    signals: u32,
    timeout_ns: Option<u64>,
) -> Result<u32, IpcError> {
    let runtime = runtime();
    let table = runtime.current_handle_table().ok_or(IpcError::BadHandle)?;

    // Берём KObject под локом таблицы и сразу отпускаем лок.
    let object = table.with_lock(|tbl| tbl.clone_object(handle_id, Rights::WAIT))?;
    let signal_state = object.signals().ok_or(IpcError::WrongType)?;

    wait_until(signal_state, signals, timeout_ns)
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

/// Закрывает handle: изымает его из таблицы и дропает (последний `Arc`
/// на KO - закрывает объект). На несуществующем id - `BadHandle`.
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
/// Имя пользователя в текущей реализации не сохраняется.
pub fn create_empty_process(name: &'static str) -> Result<Arc<ProcessObject>, SpawnError> {
    runtime().create_empty_process(name)
}

/// Создаёт user-поток в указанном процессе и помещает его в ready-queue.
pub fn create_user_thread(
    process: &Arc<ProcessObject>,
    entry: UserThreadEntry,
) -> Result<Arc<ThreadObject>, SpawnError> {
    runtime().create_user_thread(process, entry)
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
    mb.subscribe(&source, koid, key, mask, mode);
    Ok(())
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
    use std::panic;

    use super::*;
    use crate::{
        ProcessObject, ThreadObject,
        runtime::{KernelRuntime, UserThreadEntry, WaitToken, install_runtime},
    };

    const PANIC_SENTINEL: &str = "thread_exit-mock-noreturn";
    static CAPTURED_EXIT_CODE: AtomicI32 = AtomicI32::new(i32::MIN);

    struct MockRuntime;

    impl KernelRuntime for MockRuntime {
        fn current_wait_token(&self) -> WaitToken {
            WaitToken::new(NonZeroU64::new(1).unwrap())
        }

        fn current_handle_table(&self) -> Option<Arc<collections::MutexCell<crate::HandleTable>>> {
            None
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

        fn block_current_until(&self, _ready_flag: &AtomicU32, _timeout_ns: Option<u64>) {}

        fn unblock(&self, _token: WaitToken) {}

        fn create_empty_process(
            &self,
            _name: &'static str,
        ) -> Result<Arc<ProcessObject>, SpawnError> {
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
    }

    #[test]
    fn thread_exit_forwards_code_to_runtime() {
        install_runtime(Arc::new(MockRuntime));

        let prev_hook = panic::take_hook();
        panic::set_hook(Box::new(|_| {}));
        let result = panic::catch_unwind(|| thread_exit(7));
        panic::set_hook(prev_hook);

        assert!(result.is_err());
        assert_eq!(CAPTURED_EXIT_CODE.load(Ordering::SeqCst), 7);
    }
}
