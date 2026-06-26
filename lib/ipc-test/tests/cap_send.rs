//! Send-сторона cap: wire-id попадает в кадр, владение отдаётся при успешной
//! передаче (relinquish), иначе закрывается через Drop.

use std::{cell::Cell, rc::Rc, thread};

use ipc::{
    IntoWireHandle, Transport,
    wire::{Cap, IpcError},
};
use ipc_test::MockEnd;

#[ipc::protocol(name = "CapProto")]
trait CapProto {
    #[cast]
    fn send_cap(&self, cap: ipc::wire::Cap);

    #[call]
    fn exchange(&self, cap: ipc::wire::Cap) -> u64;
}

/// Исход владения cap-источником.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Fate {
    Pending,
    Relinquished,
    Dropped,
}

/// Источник cap, фиксирующий исход: relinquish (отдан ядру) или Drop (закрыт).
struct FakeHandle {
    id: u32,
    fate: Rc<Cell<Fate>>,
}

impl IntoWireHandle for FakeHandle {
    fn wire_id(&self) -> u32 {
        self.id
    }

    fn relinquish(self) {
        self.fate.set(Fate::Relinquished);
        core::mem::forget(self);
    }
}

impl Drop for FakeHandle {
    fn drop(&mut self) {
        self.fate.set(Fate::Dropped);
    }
}

/// Сервис двунаправленного обмена: возвращает id принятого cap.
struct Echo;

impl CapProtoService for Echo {
    fn send_cap(&mut self, _cap: Cap) {}

    fn exchange(&mut self, cap: Cap) -> u64 {
        u64::from(cap.raw().get())
    }
}

fn fake(id: u32) -> (FakeHandle, Rc<Cell<Fate>>) {
    let fate = Rc::new(Cell::new(Fate::Pending));
    (
        FakeHandle {
            id,
            fate: Rc::clone(&fate),
        },
        fate,
    )
}

#[test]
fn cast_cap_relinquished_on_ok() {
    let (client_end, server_end) = MockEnd::pair();
    let client = CapProtoClient::new(client_end);
    let (handle, fate) = fake(42);

    client.send_cap(handle).expect("send ok");

    assert_eq!(fate.get(), Fate::Relinquished);
    let mut bytes = [0u8; 256];
    let mut handles = [0u32; 4];
    let len = server_end
        .read_message(&mut bytes, &mut handles)
        .expect("read");
    assert_eq!(&handles[..len.handles], &[42]);
}

#[test]
fn cast_cap_dropped_on_peer_closed() {
    let (client_end, server_end) = MockEnd::pair();
    drop(server_end);
    let client = CapProtoClient::new(client_end);
    let (handle, fate) = fake(7);

    assert_eq!(client.send_cap(handle), Err(IpcError::PeerClosed));
    assert_eq!(fate.get(), Fate::Dropped);
}

#[test]
fn call_cap_dropped_on_peer_closed() {
    let (client_end, server_end) = MockEnd::pair();
    drop(server_end);
    let client = CapProtoClient::new(client_end);
    let (handle, fate) = fake(5);

    assert_eq!(client.exchange(handle), Err(IpcError::PeerClosed));
    assert_eq!(fate.get(), Fate::Dropped);
}

#[test]
fn call_cap_relinquished_on_ok() {
    let (client_end, server_end) = MockEnd::pair();
    let server = thread::spawn(move || {
        let mut echo = Echo;
        server_end
            .wait_readable(1_000_000_000)
            .expect("server wait");
        dispatch_cap_proto(&mut echo, &server_end).expect("dispatch");
    });

    let client = CapProtoClient::new(client_end);
    let (handle, fate) = fake(42);

    let reply = client.exchange(handle).expect("exchange ok");

    assert_eq!(reply, 42);
    assert_eq!(fate.get(), Fate::Relinquished);
    server.join().expect("server join");
}
