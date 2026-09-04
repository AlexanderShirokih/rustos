//! Reply-сторона cap: сервис возвращает `impl IntoWireHandle`, dispatch отдаёт
//! его при успешной reply-передаче (relinquish). Клиент получает Cap с тем же
//! wire-id.

#![allow(clippy::expect_used)]

use std::{
    sync::{Arc, Mutex},
    thread,
};

use ipc::{IntoWireHandle, Transport};
use ipc_test::MockEnd;

ipc::protocol! {
    // Источник capability: ответ отдаёт владение хэндлом.
    mod cap_source {
        name = "CapSource";

        call fn get_cap() -> Cap;
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Fate {
    Pending,
    Relinquished,
    Dropped,
}

struct FakeHandle {
    id: u32,
    fate: Arc<Mutex<Fate>>,
}

impl IntoWireHandle for FakeHandle {
    fn wire_id(&self) -> u32 {
        self.id
    }

    fn relinquish(self) {
        *self.fate.lock().expect("mutex") = Fate::Relinquished;
        core::mem::forget(self);
    }
}

impl Drop for FakeHandle {
    fn drop(&mut self) {
        *self.fate.lock().expect("mutex") = Fate::Dropped;
    }
}

struct Source {
    id: u32,
    fate: Arc<Mutex<Fate>>,
}

impl cap_source::Service for Source {
    fn get_cap(&mut self) -> impl IntoWireHandle {
        FakeHandle {
            id: self.id,
            fate: Arc::clone(&self.fate),
        }
    }
}

#[test]
fn reply_cap_relinquished_on_ok() {
    let (client_end, server_end) = MockEnd::pair();
    let fate = Arc::new(Mutex::new(Fate::Pending));
    let server = {
        let fate = Arc::clone(&fate);
        thread::spawn(move || {
            let mut src = Source { id: 77, fate };
            server_end
                .wait_readable(ipc::Timeout::from_secs(1))
                .expect("server wait");
            cap_source::dispatch(&mut src, &server_end).expect("dispatch");
        })
    };

    let client = cap_source::Client::new(client_end);
    let cap = client.get_cap().expect("get_cap ok");

    assert_eq!(cap.raw().get(), 77);
    server.join().expect("join");
    assert_eq!(*fate.lock().expect("mutex"), Fate::Relinquished);
}
