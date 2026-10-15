//! Хаб ввода держит устройства источников и раздаёт их кадры. Протокол `InputInject` объявляет
//! устройства и применяет их команды, протокол `Input` отдаёт сессии очередь кадров видимых ей
//! устройств и дескриптор устройства по номеру. Протокол `InputFocus` открывает держателю фокуса
//! сессии `Input` и выбирает, какой из них видны устройства. Каждому видимому экспорту
//! `InputSource` хаб передаёт новую сессию `InputInject`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::{boxed::Box, rc::Rc};
use core::{cell::RefCell, fmt};

use component_startup::{
    Polled, Requirements as _, SessionEvent, SessionPort, SessionRequest, StartupError, accept,
    component_abi::AcceptError, component_manifest, component_startup as startup_contract,
    serve_export, whole_export,
};
use executor::{Channel, Executor, Reactor, Receiver, Sender, channel, yield_now};
use input_abi::{input, input_focus, input_inject, input_source};
use ipc::CallError;
use runtime::{PortTransport, spawn_bound};
use syscall::{Handle, READABLE, SyscallError, Timeout, WaitItem, kind};

mod hub;
mod queue;
mod sources;

use hub::{Consumer, Holder, Hub, Source, open_input};
use sources::attach_sources;

const FRAMES_PER_TURN: usize = 16;

component_manifest! {
    struct HubStartup {
        requirements: { sources: all(input_source::Protocol) },
        exports: [input_inject::Protocol, input::Protocol, input_focus::Protocol],
    }
}

#[derive(Debug)]
enum Stopped {
    Runtime(kernel_objects::Error),
    ConsumerPortClosed,
    SourcePortClosed,
    FocusPortClosed,
    InputExport(CallError<AcceptError>),
    InputRequestsClosed,
    SourceWatch(StartupError),
}

impl fmt::Display for Stopped {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Runtime(error) => error.fmt(formatter),
            Self::ConsumerPortClosed => formatter.write_str("consumer port closed"),
            Self::SourcePortClosed => formatter.write_str("source port closed"),
            Self::FocusPortClosed => formatter.write_str("focus port closed"),
            Self::InputExport(error) => write!(formatter, "input export {error:?}"),
            Self::InputRequestsClosed => formatter.write_str("input requests closed"),
            Self::SourceWatch(error) => write!(formatter, "source watch: {error}"),
        }
    }
}

impl From<kernel_objects::Error> for Stopped {
    fn from(error: kernel_objects::Error) -> Self {
        Self::Runtime(error)
    }
}

impl From<SyscallError> for Stopped {
    fn from(error: SyscallError) -> Self {
        Self::Runtime(error.into())
    }
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Stopped> {
    let requirements = HubStartup::from_parent(parent);
    let sources: &'static SessionPort = Box::leak(Box::new(SessionPort::create()?));
    let consumers: &'static SessionPort = Box::leak(Box::new(SessionPort::create()?));
    let holders: &'static SessionPort = Box::leak(Box::new(SessionPort::create()?));
    let executor = Executor::new()?;
    let reactor = executor.reactor();
    let hub = Rc::new(RefCell::new(Hub::new()));
    let Channel {
        sender: requests,
        receiver: accepted,
    } = channel();

    // Потоки экспортов и подключения работают до конца процесса.
    let consumers_parent = requirements.parent;
    let watched = requirements.sources;
    let sources_parent = startup_contract::Client::new(PortTransport::new(parent));
    let holders_parent = startup_contract::Client::new(PortTransport::new(parent));
    let _ = spawn_bound(move || accept_input(consumers_parent, &requests))?;
    let _ = spawn_bound(move || {
        serve_export::<input_inject::Protocol>(sources_parent, whole_export(sources))
    })?;
    let _ = spawn_bound(move || {
        serve_export::<input_focus::Protocol>(holders_parent, whole_export(holders))
    })?;
    let _ = spawn_bound(move || Stopped::SourceWatch(attach_sources(&watched, sources)))?;

    let mut source = Source { hub: hub.clone() };
    executor.spawn(stop_on(serve(
        reactor.clone(),
        sources,
        Stopped::SourcePortClosed,
        move |event| match event {
            SessionEvent::Frame(frame) => drop(input_inject::dispatch(&mut source, frame)),
            SessionEvent::Gone(badge) => source.gone(badge),
        },
    )));
    let mut holder = Holder {
        hub: hub.clone(),
        consumers,
    };
    executor.spawn(stop_on(serve(
        reactor.clone(),
        holders,
        Stopped::FocusPortClosed,
        move |event| match event {
            SessionEvent::Frame(frame) => drop(input_focus::dispatch(&mut holder, frame)),
            SessionEvent::Gone(badge) => holder.gone(badge),
        },
    )));
    let mut consumer = Consumer { hub: hub.clone() };
    executor.spawn(stop_on(open_inputs(accepted, consumers, hub)));

    Err(executor.block_on(serve(
        reactor,
        consumers,
        Stopped::ConsumerPortClosed,
        move |event| match event {
            SessionEvent::Frame(frame) => drop(input::dispatch(&mut consumer, frame)),
            SessionEvent::Gone(badge) => consumer.gone(badge),
        },
    )))
}

fn accept_input(
    parent: startup_contract::Client<PortTransport>,
    requests: &Sender<SessionRequest>,
) -> Stopped {
    let parent = parent.with_wait(Timeout::INFINITE);
    loop {
        match accept::<input::Protocol>(&parent) {
            Ok(request) => {
                if requests.send(request).is_err() {
                    return Stopped::InputRequestsClosed;
                }
            }
            Err(error) => return Stopped::InputExport(error),
        }
    }
}

async fn open_inputs(
    mut accepted: Receiver<SessionRequest>,
    consumers: &'static SessionPort,
    hub: Rc<RefCell<Hub>>,
) -> Stopped {
    while let Some(request) = accepted.recv().await {
        let _ = request.reply.send(open_input(&hub, consumers));
        yield_now().await;
    }
    Stopped::InputRequestsClosed
}

async fn serve(
    reactor: Reactor,
    port: &'static SessionPort,
    closed: Stopped,
    mut handle: impl FnMut(SessionEvent<'_>),
) -> Stopped {
    let readable = WaitItem::new(port.port().as_raw(), READABLE);
    let mut events = port.events();
    let mut served = 0usize;

    loop {
        match events.try_recv() {
            Polled::Event(event) => {
                handle(event);
                served = served.saturating_add(1);
                if served >= FRAMES_PER_TURN {
                    served = 0;
                    yield_now().await;
                }
            }
            Polled::Empty => {
                served = 0;
                if let Err(error) = reactor.wait(readable).await {
                    return error.into();
                }
            }
            Polled::Closed => return closed,
        }
    }
}

async fn stop_on(task: impl Future<Output = Stopped>) {
    runtime::finish(Err::<(), _>(task.await))
}
