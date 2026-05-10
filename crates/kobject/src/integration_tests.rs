//! Хост-интеграционные тесты kobject.
//!
//! Эмулируют RPC-обмен между двумя "процессами" (двумя `HandleTable`)
//! через одну пару `Channel` + переданный handle на `Event`,
//! без интеграции со scheduler.

use alloc::sync::Arc;

use super::{
    AsyncMode, CHANNEL_READABLE, Channel, EVENT_SIGNALED, Event, Handle, HandleTable, KObject,
    MAILBOX_PAYLOAD_SIZE, Mailbox, MailboxPacket, MailboxPacketKind, Message, Rights,
    wait::{MockWaker, SignalSource},
};

/// "Сервер" получает запрос с переданным handle на ответ-Event,
/// сигналит его + пишет ответный payload обратно в канал. Клиент
/// проверяет, что сигнал поднялся на его собственном Arc<Event>,
/// и что ответ пришёл по тому же каналу.
#[test]
fn pilot_rpc_full_cycle() {
    let mut server_table = HandleTable::new();
    let mut client_table = HandleTable::new();

    let (server_end, client_end) = Channel::create_pair(8);
    let server_chan_id = server_table
        .insert(Handle::new(
            KObject::Channel(server_end.clone()),
            Rights::defaults_for(&KObject::Channel(server_end.clone())),
        ))
        .unwrap();
    let client_chan_id = client_table
        .insert(Handle::new(
            KObject::Channel(client_end.clone()),
            Rights::defaults_for(&KObject::Channel(client_end.clone())),
        ))
        .unwrap();

    // Клиент создаёт reply-Event и регистрирует у себя.
    let reply_event = Event::new();
    let reply_id = client_table
        .insert(Handle::new(
            KObject::Event(reply_event.clone()),
            Rights::defaults_for(&KObject::Event(reply_event.clone())),
        ))
        .unwrap();

    // Клиент атомарно изымает handle из своей таблицы и упаковывает в сообщение.
    let transferred = client_table.remove(reply_id).unwrap();
    let mut req = Message::from_bytes(b"ping").unwrap();
    req.push_handle(transferred).unwrap();

    // Резолвим канал клиента и пишем.
    {
        let chan = client_table
            .get_channel(client_chan_id, Rights::WRITE)
            .unwrap();
        chan.write(req).unwrap();
    }

    // Сервер читает.
    let mut received = {
        let chan = server_table
            .get_channel(server_chan_id, Rights::READ)
            .unwrap();
        chan.read().unwrap()
    };
    assert_eq!(received.bytes(), b"ping");

    // Сервер регистрирует переданный handle у себя и сигналит.
    let mut received_handles = received.drain_handles();
    let reply_handle = received_handles.next().expect("reply handle present");
    assert!(received_handles.next().is_none());
    drop(received_handles);

    let server_reply_id = server_table.insert(reply_handle).unwrap();
    {
        let event = server_table
            .get_event(server_reply_id, Rights::SIGNAL)
            .unwrap();
        event.signal(EVENT_SIGNALED, 0);
    }

    // Сервер пишет ответный payload в канал.
    {
        let chan = server_table
            .get_channel(server_chan_id, Rights::WRITE)
            .unwrap();
        chan.write(Message::from_bytes(b"pong").unwrap()).unwrap();
    }

    // Клиент видит сигнал на своём Arc<Event>.
    assert_eq!(reply_event.peek() & EVENT_SIGNALED, EVENT_SIGNALED);

    // Клиент читает ответ из канала.
    let resp = {
        let chan = client_table
            .get_channel(client_chan_id, Rights::READ)
            .unwrap();
        chan.read().unwrap()
    };
    assert_eq!(resp.bytes(), b"pong");
}

/// Обратный сценарий: клиент успевает зарегистрировать waiter на
/// `CHANNEL_READABLE` *до* того, как сервер ответит, и должен быть
/// разбужен ровно одним вызовом `signal` через write-путь канала.
#[test]
fn waiter_woken_through_channel_write() {
    let (server_end, client_end) = Channel::create_pair(4);

    let waker = MockWaker::new();
    client_end
        .signals()
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

    let (server_end, client_end) = Channel::create_pair(4);
    let id = {
        let ko = KObject::Channel(server_end);
        let rights = Rights::defaults_for(&ko);
        owner.insert(Handle::new(ko, rights)).unwrap()
    };

    let waker = MockWaker::new();
    client_end
        .signals()
        .register_waiter(super::CHANNEL_PEER_CLOSED, waker.clone());

    // Изымаем единственный Arc<Channel> из таблицы и дропаем.
    drop(owner.remove(id).unwrap());

    assert!(waker.was_woken());
    let _ = Arc::clone(&client_end);
}

/// Mailbox подписан через handle-table на сигналы Event'а: signal у
/// event'а доставляет один пакет; повторный signal после отзыва
/// подписки пакета не приносит.
#[test]
fn mailbox_async_subscribe_deliver_and_cancel() {
    let mut table = HandleTable::new();

    let mailbox = Mailbox::new();
    let mb_id = {
        let ko = KObject::Mailbox(mailbox.clone());
        let rights = Rights::defaults_for(&ko);
        table.insert(Handle::new(ko, rights)).unwrap()
    };

    let event = Event::new();
    let ev_id = {
        let ko = KObject::Event(event.clone());
        let rights = Rights::defaults_for(&ko);
        table.insert(Handle::new(ko, rights)).unwrap()
    };

    // Pre-fill один user-пакет, чтобы убедиться, что signal-пакет
    // приземляется в FIFO после него.
    mailbox
        .queue(MailboxPacket::user(0, [0u8; MAILBOX_PAYLOAD_SIZE]))
        .unwrap();

    // Эмулируем mailbox_wait_async без runtime: получаем target напрямую.
    let event_arc = table.get_event(ev_id, Rights::WAIT).unwrap();
    let mb_arc = table.get_mailbox(mb_id, Rights::WRITE).unwrap();
    let target: Arc<dyn SignalSource> = event_arc.clone();
    let target_koid = KObject::Event(event_arc.clone()).koid();
    mb_arc.subscribe(&target, target_koid, 1234, EVENT_SIGNALED, AsyncMode::Once);

    event.signal(EVENT_SIGNALED, 0);

    // user(0), потом signal(1234).
    let p1 = mailbox.try_pop().unwrap();
    assert_eq!(p1.kind, MailboxPacketKind::User);
    let p2 = mailbox.try_pop().unwrap();
    assert_eq!(p2.kind, MailboxPacketKind::SignalOnce);
    assert_eq!(p2.key, 1234);
    assert!(mailbox.try_pop().is_err());

    // Cancel идемпотентен.
    mb_arc.cancel_subscription(target_koid, 1234);
    mb_arc.cancel_subscription(target_koid, 1234);

    // Повторный signal - пакета не будет (Once уже отработал).
    event.signal(0, EVENT_SIGNALED);
    event.signal(EVENT_SIGNALED, 0);
    assert!(mailbox.try_pop().is_err());
}

/// Repeating-подписка через handle-table: каждый signal target'а кладёт
/// в mailbox по пакету; cancel останавливает доставку.
#[test]
fn mailbox_async_repeating_subscribe_and_cancel() {
    let mut table = HandleTable::new();

    let mailbox = Mailbox::new();
    let mb_id = {
        let ko = KObject::Mailbox(mailbox.clone());
        let rights = Rights::defaults_for(&ko);
        table.insert(Handle::new(ko, rights)).unwrap()
    };

    let event = Event::new();
    let ev_id = {
        let ko = KObject::Event(event.clone());
        let rights = Rights::defaults_for(&ko);
        table.insert(Handle::new(ko, rights)).unwrap()
    };

    let event_arc = table.get_event(ev_id, Rights::WAIT).unwrap();
    let mb_arc = table.get_mailbox(mb_id, Rights::WRITE).unwrap();
    let target: Arc<dyn SignalSource> = event_arc.clone();
    let target_koid = KObject::Event(event_arc.clone()).koid();
    mb_arc.subscribe(
        &target,
        target_koid,
        4242,
        EVENT_SIGNALED,
        AsyncMode::Repeating,
    );

    event.signal(EVENT_SIGNALED, EVENT_SIGNALED);
    event.signal(EVENT_SIGNALED, EVENT_SIGNALED);

    let p1 = mailbox.try_pop().unwrap();
    assert_eq!(p1.kind, MailboxPacketKind::SignalRepeating);
    assert_eq!(p1.key, 4242);
    let p2 = mailbox.try_pop().unwrap();
    assert_eq!(p2.kind, MailboxPacketKind::SignalRepeating);
    assert!(mailbox.try_pop().is_err());

    mb_arc.cancel_subscription(target_koid, 4242);

    event.signal(EVENT_SIGNALED, EVENT_SIGNALED);
    assert!(mailbox.try_pop().is_err());
}
