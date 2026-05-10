//! Round-trip через `Channel`: сигналы READABLE/PEER_CLOSED.

use test_harness_qemu::register_test;

fn channel_echo() {
    use kobject::{CHANNEL_PEER_CLOSED, CHANNEL_READABLE, Channel, IpcError, Message};

    let (a, b) = Channel::create_pair(4);

    // Пустая очередь -> ShouldWait.
    test_harness_qemu::kassert!(matches!(b.read(), Err(IpcError::ShouldWait)));

    a.write(Message::from_bytes(b"echo-payload").expect("payload fits"))
        .expect("write succeeds");
    test_harness_qemu::kassert!(b.peek_signals() & CHANNEL_READABLE != 0);

    let msg = b.read().expect("read succeeds");
    test_harness_qemu::kassert_eq!(msg.bytes(), b"echo-payload");
    test_harness_qemu::kassert!(b.peek_signals() & CHANNEL_READABLE == 0);

    // Drop одного эндпоинта поднимает PEER_CLOSED на втором.
    drop(a);
    test_harness_qemu::kassert!(b.peek_signals() & CHANNEL_PEER_CLOSED != 0);
    test_harness_qemu::kassert!(matches!(b.read(), Err(IpcError::PeerClosed)));
}

register_test!(CHANNEL_ECHO, "channel_echo", channel_echo);
