//! Round-trip через `Channel`: сигналы READABLE/PEER_CLOSED.

use kernel_tests::kernel_test;

#[kernel_test]
fn channel_echo() {
    use kobject::{CHANNEL_PEER_CLOSED, CHANNEL_READABLE, Channel, IpcError, Message};

    let (a, b) = Channel::create_pair(4);

    // Пустая очередь -> ShouldWait.
    kernel_tests::kassert!(matches!(b.read(), Err(IpcError::ShouldWait)));

    a.write(Message::from_bytes(b"echo-payload").expect("payload fits"))
        .expect("write succeeds");
    kernel_tests::kassert!(b.peek_signals() & CHANNEL_READABLE != 0);

    let msg = b.read().expect("read succeeds");
    kernel_tests::kassert_eq!(msg.bytes(), b"echo-payload");
    kernel_tests::kassert!(b.peek_signals() & CHANNEL_READABLE == 0);

    // Drop одного эндпоинта поднимает PEER_CLOSED на втором.
    drop(a);
    kernel_tests::kassert!(b.peek_signals() & CHANNEL_PEER_CLOSED != 0);
    kernel_tests::kassert!(matches!(b.read(), Err(IpcError::PeerClosed)));
}
