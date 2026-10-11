//! Хаб ввода держит устройства источников и раздаёт их кадры. Протокол `InputInject` объявляет
//! устройства и применяет их команды, протокол `Input` отдаёт сессии очередь кадров всех
//! устройств и дескриптор устройства по номеру. Каждому видимому экспорту `InputSource` хаб
//! передаёт новую сессию `InputInject`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use core::fmt;

use component_startup::{
    Requirements as _, SessionEvent, SessionPort, StartupError, component_manifest,
    component_startup as startup_contract, serve_export, whole_export,
};
use input_abi::{input, input_inject, input_source};
use runtime::{PortTransport, spawn_bound};
use syscall::{Handle, SyscallError, kind};

mod hub;
mod queue;
mod sources;

use hub::{Consumer, Source, input_gone, open_input, source_gone};
use sources::attach_sources;

component_manifest! {
    struct HubStartup {
        requirements: { sources: all(input_source::Protocol) },
        exports: [input_inject::Protocol, input::Protocol],
    }
}

#[derive(Debug)]
enum Stopped {
    Runtime(kernel_objects::Error),
    ConsumerPortClosed,
    SourcePortClosed,
    SourceWatch(StartupError),
}

impl fmt::Display for Stopped {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Runtime(error) => error.fmt(formatter),
            Self::ConsumerPortClosed => formatter.write_str("consumer port closed"),
            Self::SourcePortClosed => formatter.write_str("source port closed"),
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

    // Потоки источников, экспортов и подключения работают до конца процесса.
    let consumers_parent = requirements.parent;
    let watched = requirements.sources;
    let sources_parent = startup_contract::Client::new(PortTransport::new(parent));
    let _ = spawn_bound(move || {
        let mut events = sources.events();

        while let Some(event) = events.recv() {
            match event {
                SessionEvent::Frame(frame) => {
                    let _ = input_inject::dispatch(&mut Source, frame);
                }
                SessionEvent::Gone(badge) => source_gone(badge),
            }
        }
        Stopped::SourcePortClosed
    })?;
    let _ = spawn_bound(move || {
        serve_export::<input_inject::Protocol>(sources_parent, whole_export(sources))
    })?;
    let _ = spawn_bound(move || {
        serve_export::<input::Protocol>(consumers_parent, |_| open_input(consumers))
    })?;
    let _ = spawn_bound(move || Stopped::SourceWatch(attach_sources(&watched, sources)))?;

    let mut events = consumers.events();

    while let Some(event) = events.recv() {
        match event {
            SessionEvent::Frame(frame) => {
                let _ = input::dispatch(&mut Consumer, frame);
            }
            SessionEvent::Gone(badge) => input_gone(badge),
        }
    }

    Err(Stopped::ConsumerPortClosed)
}
