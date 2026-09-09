//! Send-сторона cap: wire-id попадает в кадр, владение отдаётся при успешной
//! передаче (relinquish), иначе закрывается через Drop.

#![allow(clippy::std_instead_of_core)]

use std::{cell::Cell, rc::Rc, thread};

use ipc::{
    IntoWireHandle, Transport,
    wire::{Cap, IpcError},
};
use ipc_test::MockEnd;
use syscall::kind;

ipc::protocol! {
    // Контракт передачи capability в запросе.
    mod cap_proto {
        name = "CapProto";

        cast fn send_cap(cap: Cap<kind::Port>);
        call fn exchange(cap: Cap<kind::MemoryRegion>) -> u64;
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Fate {
    Pending,
    Relinquished,
    Dropped,
}

struct FakeHandle<K: syscall::ObjectKind> {
    id: u32,
    fate: Rc<Cell<Fate>>,
    kind: core::marker::PhantomData<K>,
}

impl<K: syscall::ObjectKind> IntoWireHandle for FakeHandle<K> {
    type Kind = K;

    fn wire_id(&self) -> u32 {
        self.id
    }

    fn relinquish(self) {
        self.fate.set(Fate::Relinquished);
        core::mem::forget(self);
    }
}

impl<K: syscall::ObjectKind> Drop for FakeHandle<K> {
    fn drop(&mut self) {
        self.fate.set(Fate::Dropped);
    }
}

struct Echo;

impl cap_proto::Service for Echo {
    fn send_cap(&mut self, _cap: Cap<kind::Port>) {}

    fn exchange(&mut self, cap: Cap<kind::MemoryRegion>) -> u64 {
        u64::from(cap.raw().get())
    }
}

fn fake<K: syscall::ObjectKind>(id: u32) -> (FakeHandle<K>, Rc<Cell<Fate>>) {
    let fate = Rc::new(Cell::new(Fate::Pending));
    (
        FakeHandle {
            id,
            fate: Rc::clone(&fate),
            kind: core::marker::PhantomData,
        },
        fate,
    )
}

#[test]
fn cast_cap_relinquished_on_ok() {
    let (client_end, server_end) = MockEnd::pair();
    let client = cap_proto::Client::new(client_end);
    let (handle, fate) = fake::<kind::Port>(42);

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
    let client = cap_proto::Client::new(client_end);
    let (handle, fate) = fake::<kind::Port>(7);

    assert_eq!(client.send_cap(handle), Err(IpcError::PeerClosed));
    assert_eq!(fate.get(), Fate::Dropped);
}

#[test]
fn call_cap_dropped_on_peer_closed() {
    let (client_end, server_end) = MockEnd::pair();
    drop(server_end);
    let client = cap_proto::Client::new(client_end);
    let (handle, fate) = fake::<kind::MemoryRegion>(5);

    assert_eq!(
        client.exchange(handle).map(|response| response.value),
        Err(IpcError::PeerClosed)
    );
    assert_eq!(fate.get(), Fate::Dropped);
}

#[test]
fn call_cap_relinquished_on_ok() {
    let (client_end, server_end) = MockEnd::pair();
    let server = thread::spawn(move || {
        let mut echo = Echo;
        server_end
            .wait_readable(ipc::Timeout::from_secs(1))
            .expect("server wait");
        cap_proto::dispatch(&mut echo, &server_end).expect("dispatch");
    });

    let client = cap_proto::Client::new(client_end);
    let (handle, fate) = fake::<kind::MemoryRegion>(42);

    let response = client.exchange(handle).expect("exchange ok");

    assert_eq!(response.value, 42);
    assert_eq!(fate.get(), Fate::Relinquished);
    server.join().expect("server join");
}
