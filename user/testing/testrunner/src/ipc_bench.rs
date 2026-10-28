//! Замеры цены одного вызова контракта при синхронном и асинхронном ожидании клиента и при
//! синхронном и задачном обслуживании сервера. Прогон под TCG не моделирует кеши процессора.

use alloc::rc::Rc;
use core::{future::Future, num::NonZeroU64};

use component_startup::{SessionPort, serve_calls};
use executor::PortChannel;
use ipc::{
    Incoming, Listener as _,
    wire::{IpcError, MESSAGE_INLINE_MAX},
};
use kernel_objects::{Delivery, Port, Signal};
use runtime::{Instant, JoinHandle, PortTransport, spawn_owned};
use syscall::{SIGNALED, WaitItem};

use crate::fixture::JOIN_TIMEOUT;

const ROUNDS: u64 = 256;

const BADGE: NonZeroU64 = NonZeroU64::MIN;

ipc::protocol! {
    pub mod echo_now {
        name = "BenchEcho";

        call fn echo(value: u32) -> u32;
    }
}

ipc::protocol! {
    pub mod echo_later {
        name = "BenchEcho";

        call async fn echo(value: u32) -> u32;
    }
}

struct Echo;

impl echo_now::Service<Delivery> for Echo {
    fn echo(&self, _frame: &Delivery, value: u32) -> u32 {
        value
    }
}

#[allow(clippy::unused_async_trait_impl)]
impl echo_later::Service<Delivery> for Echo {
    async fn echo(&self, _frame: &Delivery, value: u32) -> u32 {
        value
    }
}

#[derive(Clone, Copy)]
enum Server {
    Inline,
    Tasks,
    AsyncTasks,
}

impl Server {
    const fn name(self) -> &'static str {
        match self {
            Self::Inline => "inline",
            Self::Tasks => "tasks",
            Self::AsyncTasks => "async_tasks",
        }
    }
}

fn elapsed_per_op(start: Instant) -> u64 {
    Instant::now().as_nanos().saturating_sub(start.as_nanos()) / ROUNDS
}

fn serve_inline(server: &Port) -> u32 {
    let listener = PortTransport::new(server.as_raw());
    let mut bytes = [0; MESSAGE_INLINE_MAX];

    loop {
        match listener.recv(&mut bytes, JOIN_TIMEOUT) {
            Ok(frame) => {
                let _ = echo_now::dispatch(&Echo, frame);
            }
            Err(_) => return 0,
        }
    }
}

fn serve_tasks<F: Future<Output = Result<(), IpcError>> + 'static>(
    sessions: SessionPort,
    handle: impl Fn(Incoming<'_, Delivery>) -> Result<Option<F>, IpcError> + 'static,
) -> u32 {
    let executor = Rc::new(crate::executor::executor());
    let stop = Rc::new(Signal::create().expect("stop signal"));
    let stopping = stop.clone();
    let serving = serve_calls(&executor, sessions, handle, move |_| {
        let _ = stopping.notify_all();
    });
    let _serving = executor.spawn_now(serving).abort_on_drop();

    let _ = executor.block_on(executor::wait(WaitItem::new(stop.as_raw(), SIGNALED)));
    0
}

fn start(server: Server) -> (Port, JoinHandle) {
    match server {
        Server::Inline => {
            let service = Port::create_service(Some(BADGE)).expect("service port");
            let server = service.server;
            let thread = spawn_owned(move || serve_inline(&server)).expect("server");
            (service.client, thread)
        }
        Server::Tasks | Server::AsyncTasks => {
            let sessions = SessionPort::create().expect("session port");
            let client = sessions.create_session().expect("session").port;
            let thread = spawn_owned(move || {
                let echo = Rc::new(Echo);
                match server {
                    Server::Tasks => {
                        serve_tasks(sessions, move |frame| echo_now::serve(&echo, frame))
                    }
                    _ => serve_tasks(sessions, move |frame| echo_later::serve(&echo, frame)),
                }
            })
            .expect("server");
            (client, thread)
        }
    }
}

fn measure_wait(port: &Port) -> u64 {
    let start = Instant::now();
    let client = echo_now::Client::new(PortTransport::new(port.as_raw()));
    for _ in 0..ROUNDS {
        test_harness::check_eq!(client.echo(7).wait(), Ok(7));
    }

    elapsed_per_op(start)
}

fn measure_await(port: &Port) -> u64 {
    let executor = crate::executor::executor();
    let start = Instant::now();
    let client = echo_now::Client::new(PortChannel::new(port.as_raw()));
    executor.block_on(async {
        for _ in 0..ROUNDS {
            test_harness::check_eq!(client.echo(7).await, Ok(7));
        }
    });

    elapsed_per_op(start)
}

#[test_harness::test(bench)]
fn bench_ipc_call_forms() {
    for server in [Server::Inline, Server::Tasks, Server::AsyncTasks] {
        let (port, thread) = start(server);
        let wait = measure_wait(&port);
        let awaited = measure_await(&port);
        drop(port);

        thread.join(JOIN_TIMEOUT).expect("server joins");
        test_harness::report!(
            "ipc echo server={} wait_ns_per_op={wait} await_ns_per_op={awaited}",
            server.name()
        );
    }
}
