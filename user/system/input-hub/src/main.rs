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
    Requirements as _, SessionError, SessionEvent, SessionEvents, component_manifest,
    component_startup as startup_contract, serve_export,
};
use input_abi::{input, input_inject, input_source};
use kernel_objects::Port;
use runtime::{FAILED_EXIT_CODE, PortTransport, exit, log_fmt, log_line, spawn_owned};
use syscall::{Handle, SyscallError, kind};

mod hub;
mod queue;
mod sources;

use hub::{Consumer, Source, input_gone, open_input, open_source, source_gone};
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
}

impl fmt::Display for Stopped {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Runtime(error) => error.fmt(formatter),
            Self::ConsumerPortClosed => formatter.write_str("consumer port closed"),
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
    let sources: &'static Port = Box::leak(Box::new(Port::create()?));
    let consumers: &'static Port = Box::leak(Box::new(Port::create()?));

    // Потоки источников, экспортов и подключения работают до конца процесса.
    let consumers_parent = requirements.parent;
    let watched = requirements.sources;
    let sources_parent = startup_contract::Client::new(PortTransport::new(parent));
    let _ = spawn_owned(move || {
        let mut events = SessionEvents::new(sources);

        while let Some(event) = events.recv() {
            match event {
                SessionEvent::Frame(frame) => {
                    let _ = input_inject::dispatch(&mut Source, frame);
                }
                SessionEvent::Gone(badge) => source_gone(badge),
            }
        }
        log_line("source port closed");
        exit(FAILED_EXIT_CODE)
    })?;
    let _ = spawn_owned(move || {
        export::<input_inject::Protocol>(&sources_parent, sources, open_source)
    })?;
    let _ =
        spawn_owned(move || export::<input::Protocol>(&consumers_parent, consumers, open_input))?;
    let _ = spawn_owned(move || {
        let failure = attach_sources(&watched, sources);
        log_fmt(format_args!("source watch stopped: {failure}"));
        exit(FAILED_EXIT_CODE)
    })?;

    let mut events = SessionEvents::new(consumers);

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

fn export<P: ipc::Protocol>(
    parent: &startup_contract::Client<PortTransport>,
    port: &Port,
    open: fn(&Port) -> Result<Port, SessionError>,
) -> u32 {
    let failure = serve_export::<P>(parent, |_| open(port));
    log_fmt(format_args!("export stopped: {failure:?}"));
    exit(FAILED_EXIT_CODE)
}
