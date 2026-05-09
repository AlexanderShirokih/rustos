//! Round-trip через `ChannelEndpoint`: сигналы READABLE/PEER_CLOSED.

use qemu_test_harness::register_test;

fn channel_echo() {
    use kobject::{CHANNEL_PEER_CLOSED, CHANNEL_READABLE, ChannelEndpoint, IpcError, Message};

    let (a, b) = ChannelEndpoint::create_pair(4);

    // Пустая очередь -> ShouldWait.
    qemu_test_harness::kassert!(matches!(b.read(), Err(IpcError::ShouldWait)));

    a.write(Message::from_bytes(b"echo-payload").expect("payload fits"))
        .expect("write succeeds");
    qemu_test_harness::kassert!(b.peek_signals() & CHANNEL_READABLE != 0);

    let msg = b.read().expect("read succeeds");
    qemu_test_harness::kassert_eq!(msg.bytes(), b"echo-payload");
    qemu_test_harness::kassert!(b.peek_signals() & CHANNEL_READABLE == 0);

    // Drop одного эндпоинта поднимает PEER_CLOSED на втором.
    drop(a);
    qemu_test_harness::kassert!(b.peek_signals() & CHANNEL_PEER_CLOSED != 0);
    qemu_test_harness::kassert!(matches!(b.read(), Err(IpcError::PeerClosed)));
}

register_test!(CHANNEL_ECHO, "channel_echo", channel_echo);
