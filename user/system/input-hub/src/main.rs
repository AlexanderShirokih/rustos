//! Хаб ввода держит устройства источников и раздаёт их кадры. Протокол `InputInject` объявляет
//! устройства и применяет их команды, протокол `Input` отдаёт сессии очередь кадров видимых ей
//! устройств и дескриптор устройства по номеру. Протокол `InputFocus` открывает держателю фокуса
//! сессии `Input` и выбирает, какой из них видны устройства. Каждому видимому экспорту
//! `InputSource` хаб передаёт новую сессию `InputInject`.
//!
//! Экспорты, их сессии и подключение источников обслуживают задачи одного executor на потоке
//! входа.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::{boxed::Box, rc::Rc};
use core::{cell::RefCell, fmt};

use component_startup::{
    AsyncStartup, ExportStopped, Polled, Requirements as _, SessionEvent, SessionPort,
    StartupError, component_manifest, serve_export_async, whole_export,
};
use executor::{Executor, PortChannel, Reactor, yield_now};
use input_abi::{input, input_focus, input_inject, input_source};
use syscall::{Handle, READABLE, SyscallError, WaitItem, kind};

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
    Export(ExportStopped),
    SourceWatch(StartupError),
}

impl fmt::Display for Stopped {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Runtime(error) => error.fmt(formatter),
            Self::ConsumerPortClosed => formatter.write_str("consumer port closed"),
            Self::SourcePortClosed => formatter.write_str("source port closed"),
            Self::FocusPortClosed => formatter.write_str("focus port closed"),
            Self::Export(stopped) => stopped.fmt(formatter),
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
    let startup = || AsyncStartup::new(PortChannel::new(parent, reactor.clone()));

    let inputs = hub.clone();
    let input =
        serve_export_async::<input::Protocol>(startup(), move |_| open_input(&inputs, consumers));
    let inject = serve_export_async::<input_inject::Protocol>(startup(), whole_export(sources));
    let focus = serve_export_async::<input_focus::Protocol>(startup(), whole_export(holders));

    executor.spawn(stop_on(exported(input)));
    executor.spawn(stop_on(exported(inject)));
    executor.spawn(stop_on(exported(focus)));

    let watching = reactor.clone();
    executor.spawn(stop_on(async move {
        Stopped::SourceWatch(attach_sources(&requirements.sources, sources, &watching).await)
    }));

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
    let mut consumer = Consumer { hub };

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

async fn exported(export: impl Future<Output = ExportStopped>) -> Stopped {
    Stopped::Export(export.await)
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
