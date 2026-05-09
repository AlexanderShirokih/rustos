//! Полный round-trip handle-based API: `channel_create` ->
//! `channel_write`/`channel_read` -> `handle_duplicate` ->
//! `handle_close` -> ожидание `PEER_CLOSED` через `object_wait_one`.
//!
//! Прогоняется целиком через `kobject::api`-фасад, без прямых обращений
//! к `Arc<ChannelEndpoint>` - это закрепляет, что новый syscall-уровень
//! API самодостаточен для типовых сценариев IPC.

use kobject::{
    CHANNEL_PEER_CLOSED, CHANNEL_READABLE, IpcError, Message, Rights, channel_create, channel_read,
    channel_write, handle_close, handle_duplicate, object_wait_one,
};
use test_harness_qemu::register_test;

fn channel_full_api() {
    let (left, right) = channel_create().expect("channel_create must succeed");

    // Round-trip полезной нагрузки в обе стороны.
    channel_write(left, Message::from_bytes(b"ping").expect("payload fits"))
        .expect("write left -> right");
    let observed = object_wait_one(right, CHANNEL_READABLE, None).expect("wait readable on right");
    test_harness_qemu::kassert!(observed & CHANNEL_READABLE != 0);

    let msg = channel_read(right).expect("right reads ping");
    test_harness_qemu::kassert_eq!(msg.bytes(), b"ping");

    channel_write(right, Message::from_bytes(b"pong").expect("payload fits"))
        .expect("write right -> left");
    let msg = channel_read(left).expect("left reads pong");
    test_harness_qemu::kassert_eq!(msg.bytes(), b"pong");

    // duplicate: read-only копия right не должна уметь писать.
    let read_only = handle_duplicate(right, Rights::READ).expect("duplicate with subset rights");
    test_harness_qemu::kassert!(matches!(
        channel_write(read_only, Message::from_bytes(b"x").unwrap()),
        Err(IpcError::AccessDenied)
    ));

    // Закрываем left endpoint. Парный right должен увидеть PEER_CLOSED,
    // а последующий read - вернуть PeerClosed на пустой очереди.
    handle_close(left).expect("close left endpoint");

    let observed =
        object_wait_one(right, CHANNEL_PEER_CLOSED, None).expect("wait peer_closed on right");
    test_harness_qemu::kassert!(observed & CHANNEL_PEER_CLOSED != 0);
    test_harness_qemu::kassert!(matches!(channel_read(right), Err(IpcError::PeerClosed)));

    // Cleanup: дубликат и оригинальный right закрываем явно.
    handle_close(read_only).expect("close read-only dup");
    handle_close(right).expect("close right endpoint");
}

register_test!(CHANNEL_FULL_API, "channel_full_api", channel_full_api);
