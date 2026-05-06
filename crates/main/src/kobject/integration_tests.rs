//! Хост-интеграционные тесты kobject.
//!
//! Эмулируют RPC-обмен между двумя "процессами" (двумя `HandleTable`)
//! через одну пару `ChannelEndpoint` + переданный handle на `Event`,
//! без интеграции со scheduler.

use alloc::sync::Arc;

use super::{
    CHANNEL_READABLE, ChannelEndpoint, EVENT_SIGNALED, Event, Handle, HandleTable, Message,
    ObjectType, Rights, wait::MockWaker,
};

/// "Сервер" получает запрос с переданным handle на ответ-Event,
/// сигналит его + пишет ответный payload обратно в канал. Клиент
/// проверяет, что сигнал поднялся на его собственном Arc<Event>,
/// и что ответ пришёл по тому же каналу.
#[test]
fn pilot_rpc_full_cycle() {
    let mut server_table = HandleTable::new();
    let mut client_table = HandleTable::new();

    let (server_end, client_end) = ChannelEndpoint::create_pair(8);
    let server_chan_id = server_table
        .insert(Handle::new(
            server_end.clone(),
            Rights::defaults_for(ObjectType::Channel),
        ))
        .unwrap();
    let client_chan_id = client_table
        .insert(Handle::new(
            client_end.clone(),
            Rights::defaults_for(ObjectType::Channel),
        ))
        .unwrap();

    // Клиент создаёт reply-Event и регистрирует у себя.
    let reply_event = Event::new();
    let reply_id = client_table
        .insert(Handle::new(
            reply_event.clone(),
            Rights::defaults_for(ObjectType::Event),
        ))
        .unwrap();

    // Клиент атомарно изымает handle из своей таблицы и упаковывает в сообщение.
    let transferred = client_table.remove(reply_id).unwrap();
    let mut req = Message::from_bytes(b"ping").unwrap();
    req.push_handle(transferred).unwrap();

    // Резолвим канал клиента и пишем.
    {
        let h = client_table
            .get(client_chan_id, Rights::WRITE, ObjectType::Channel)
            .unwrap();
        h.object().as_channel().unwrap().write(req).unwrap();
    }

    // Сервер читает.
    let mut received = {
        let h = server_table
            .get(server_chan_id, Rights::READ, ObjectType::Channel)
            .unwrap();
        h.object().as_channel().unwrap().read().unwrap()
    };
    assert_eq!(received.bytes(), b"ping");

    // Сервер регистрирует переданный handle у себя и сигналит.
    let mut received_handles = received.drain_handles();
    let reply_handle = received_handles.next().expect("reply handle present");
    assert!(received_handles.next().is_none());
    drop(received_handles);

    let server_reply_id = server_table.insert(reply_handle).unwrap();
    {
        let h = server_table
            .get(server_reply_id, Rights::SIGNAL, ObjectType::Event)
            .unwrap();
        h.object().as_event().unwrap().signal(EVENT_SIGNALED, 0);
    }

    // Сервер пишет ответный payload в канал.
    {
        let h = server_table
            .get(server_chan_id, Rights::WRITE, ObjectType::Channel)
            .unwrap();
        h.object()
            .as_channel()
            .unwrap()
            .write(Message::from_bytes(b"pong").unwrap())
            .unwrap();
    }

    // Клиент видит сигнал на своём Arc<Event>.
    assert_eq!(reply_event.peek() & EVENT_SIGNALED, EVENT_SIGNALED);

    // Клиент читает ответ из канала.
    let resp = {
        let h = client_table
            .get(client_chan_id, Rights::READ, ObjectType::Channel)
            .unwrap();
        h.object().as_channel().unwrap().read().unwrap()
    };
    assert_eq!(resp.bytes(), b"pong");
}

/// Обратный сценарий: клиент успевает зарегистрировать waiter на
/// `CHANNEL_READABLE` *до* того, как сервер ответит, и должен быть
/// разбужен ровно одним вызовом `signal` через write-путь канала.
#[test]
fn waiter_woken_through_channel_write() {
    let (server_end, client_end) = ChannelEndpoint::create_pair(4);

    let waker = MockWaker::new();
    client_end
        .signal_state()
        .register_waiter(CHANNEL_READABLE, waker.clone());
    assert!(!waker.was_woken());

    server_end
        .write(Message::from_bytes(b"hi").unwrap())
        .unwrap();
    assert!(waker.was_woken());
    assert_eq!(waker.observed() & CHANNEL_READABLE, CHANNEL_READABLE);
}

/// Если канал-эндпоинт изъят из таблицы и его последний `Arc` дропнут,
/// парный эндпоинт получает `PEER_CLOSED` и просыпается зарегистрированный
/// waiter - даже если waiter ждал других сигналов изначально.
#[test]
fn closing_endpoint_via_table_signals_peer() {
    let mut owner = HandleTable::new();

    let (server_end, client_end) = ChannelEndpoint::create_pair(4);
    let id = owner
        .insert(Handle::new(
            server_end,
            Rights::defaults_for(ObjectType::Channel),
        ))
        .unwrap();

    let waker = MockWaker::new();
    client_end
        .signal_state()
        .register_waiter(super::CHANNEL_PEER_CLOSED, waker.clone());

    // Изымаем единственный Arc<ChannelEndpoint> из таблицы и дропаем.
    drop(owner.remove(id).unwrap());

    assert!(waker.was_woken());
    let _ = Arc::clone(&client_end);
}
