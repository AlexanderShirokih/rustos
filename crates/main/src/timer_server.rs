//! Kernel-thread, обслуживающий channel-based timer-сервис.
//!
//! Сервер на старте создаёт ровно один [`Event`] KO как сигнал
//! "дедлайн наступил" и отдаёт клиенту handle на этот event вместе с
//! handle'ом на свой conn-эндпоинт. Затем сидит в `read`-loop'е,
//! исполняя команды:
//!
//! - `SET_DEADLINE { period_ms: u64 }` - снять `TIMER_SIGNALED`,
//!   sleep на `period_ms`, поднять `TIMER_SIGNALED`.
//!
//! Один сервер обслуживает одного клиента. Параллельные таймеры и
//! cancel-операции - отложено до полноценной миграции сервиса.

extern crate alloc;

use alloc::sync::Arc;

use drivers_common::services::scheduler::{
    Priority, SchedulerService, SchedulerServiceExt, SpawnConfig,
};
use klog::{info, warn};

use crate::kobject::{
    CHANNEL_PEER_CLOSED, CHANNEL_READABLE, ChannelEndpoint, Event, Handle, IpcError, KObject,
    Message, Rights, install_handle, object_signal, object_wait_one,
};

/// Бит сигнала "timer expired". Convention протокола timer-server'а:
/// клиент wait'ит на этом бите, сервер выставляет его при наступлении
/// дедлайна.
pub const TIMER_SIGNALED: u32 = 1 << 0;

/// Заголовочный байт сообщения "установить дедлайн".
pub const CMD_SET_DEADLINE: u8 = 0x01;
/// Длина сообщения `SET_DEADLINE`: 1 байт-команда + u64 LE period_ms.
pub const SET_DEADLINE_LEN: usize = 1 + 8;

/// Кодирует `SET_DEADLINE { period_ms }` в [`Message`]-payload.
pub fn encode_set_deadline(period_ms: u64) -> Message {
    let mut bytes = [0u8; SET_DEADLINE_LEN];
    bytes[0] = CMD_SET_DEADLINE;
    bytes[1..].copy_from_slice(&period_ms.to_le_bytes());
    Message::from_bytes(&bytes).expect("SET_DEADLINE_LEN <= MESSAGE_INLINE_MAX")
}

/// Запускает kernel-thread "timer-server" и возвращает client-end канала.
/// Из этого endpoint первое сообщение содержит handle на Event KO,
/// который сервер сигналит при наступлении дедлайна.
pub fn spawn_timer_server(
    scheduler: &Arc<dyn SchedulerService>,
) -> Result<Arc<ChannelEndpoint>, &'static str> {
    let (server_end, client_end) = ChannelEndpoint::create_pair(8);

    let scheduler_for_server = scheduler.clone();
    let server_end_for_thread = server_end.clone();

    scheduler
        .spawn(
            SpawnConfig::new("timer-server").priority(Priority::highest()),
            move || run_server(&scheduler_for_server, &server_end_for_thread),
        )
        .map_err(|_| "spawn timer-server failed")?;

    Ok(client_end)
}

fn run_server(scheduler: &Arc<dyn SchedulerService>, server_end: &Arc<ChannelEndpoint>) {
    let event = Event::new();

    // Welcome-сообщение: handle на Event KO передаётся клиенту.
    let mut welcome = Message::new();
    // Клиент получает право `SIGNAL`, чтобы уметь явно очистить
    // `TIMER_SIGNALED` между итерациями (избегая "залипания" бита от
    // предыдущего fire'а).
    let event_ko = KObject::Event(event.clone());
    let event_handle = Handle::new(event_ko, Rights::WAIT | Rights::SIGNAL | Rights::INSPECT);
    if welcome.push_handle(event_handle).is_err() {
        warn!("timer-server: welcome build failed");
        return;
    }
    if let Err(e) = server_end.write(welcome) {
        warn!("timer-server: welcome write failed: {:?}", e);
        return;
    }

    // Регистрируем server-end в собственной handle-table, чтобы ходить
    // через handle-based wait API.
    let chan_ko = KObject::Channel(server_end.clone());
    let chan_handle = Handle::new(chan_ko, Rights::READ | Rights::WAIT | Rights::INSPECT);
    let chan_id = match install_handle(chan_handle) {
        Ok(id) => id,
        Err(e) => {
            warn!("timer-server: install_handle failed: {:?}", e);
            return;
        }
    };

    info!("timer-server: started, awaiting commands");

    loop {
        match object_wait_one(chan_id, CHANNEL_READABLE | CHANNEL_PEER_CLOSED, None) {
            Ok(observed)
                if observed & CHANNEL_PEER_CLOSED != 0
                    && server_end.peek_signals() & CHANNEL_READABLE == 0 =>
            {
                info!("timer-server: peer closed, exiting");
                return;
            }
            Ok(_) => {}
            Err(e) => {
                warn!("timer-server: wait failed: {:?}", e);
                return;
            }
        }

        let msg = match server_end.read() {
            Ok(msg) => msg,
            Err(IpcError::ShouldWait) => continue,
            Err(IpcError::PeerClosed) => {
                info!("timer-server: peer closed, exiting");
                return;
            }
            Err(e) => {
                warn!("timer-server: read failed: {:?}", e);
                continue;
            }
        };

        handle_command(scheduler, &event, &msg);
    }
}

fn handle_command(scheduler: &Arc<dyn SchedulerService>, event: &Arc<Event>, msg: &Message) {
    let bytes = msg.bytes();
    if bytes.len() != SET_DEADLINE_LEN || bytes[0] != CMD_SET_DEADLINE {
        warn!("timer-server: unknown command, len={}", bytes.len());
        return;
    }

    let mut period_le = [0u8; 8];
    period_le.copy_from_slice(&bytes[1..9]);
    let period_ms = u64::from_le_bytes(period_le);

    event.signal(0, TIMER_SIGNALED);
    scheduler.sleep_ms(period_ms);
    event.signal(TIMER_SIGNALED, 0);
}

/// Клиентская сторона pilot: устанавливает channel- и timer-handle'ы
/// в свою handle-table и возвращает их идентификаторы. Используется
/// демо-процессом из [`crate::kmain`].
pub fn pilot_client_subscribe(client_end: Arc<ChannelEndpoint>) -> Result<PilotHandles, IpcError> {
    let chan_ko = KObject::Channel(client_end.clone());
    let chan_id = install_handle(Handle::new(
        chan_ko,
        Rights::READ | Rights::WRITE | Rights::WAIT | Rights::INSPECT,
    ))?;

    // Первое сообщение от сервера: handle на Event-сигнал таймера.
    object_wait_one(chan_id, CHANNEL_READABLE, None)?;
    let mut welcome = client_end.read()?;

    let mut drained = welcome.drain_handles();
    let timer_handle = drained.next().ok_or(IpcError::BadHandle)?;
    if drained.next().is_some() {
        return Err(IpcError::WrongType);
    }
    drop(drained);

    match timer_handle.object() {
        KObject::Event(_) => {}
        KObject::Channel(_) => return Err(IpcError::WrongType),
    }

    let timer_id = install_handle(timer_handle)?;

    Ok(PilotHandles {
        chan_id,
        timer_id,
        client_end,
    })
}

/// Кортеж handle-id'ов плюс владеющий `Arc` на client-end канала
/// (нужен для прямого `write`; в полноценной IPC API будет
/// `channel_write(handle_id, msg)`).
pub struct PilotHandles {
    pub chan_id: crate::kobject::HandleId,
    pub timer_id: crate::kobject::HandleId,
    pub client_end: Arc<ChannelEndpoint>,
}

/// Один шаг pilot-loop'а: послать SET_DEADLINE, дождаться сигнала и
/// "потребить" его (очистить бит), чтобы следующий wait снова блокировал.
pub fn pilot_tick(handles: &PilotHandles, period_ms: u64) -> Result<(), IpcError> {
    object_signal(handles.timer_id, 0, TIMER_SIGNALED)?;
    handles.client_end.write(encode_set_deadline(period_ms))?;
    let observed = object_wait_one(handles.timer_id, TIMER_SIGNALED, None)?;
    debug_assert_eq!(observed & TIMER_SIGNALED, TIMER_SIGNALED);
    Ok(())
}
