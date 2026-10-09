//! Маршрутизатор имён разбирает пути клиентов `Namespace` по монтированиям и выдаёт сессии
//! файловых систем. Держатель `FileSystemMount` задаёт путь и файловую сессию монтирования.
//! Начальные монтирования задаёт таблица в образе (модуль `table`).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use core::fmt;

use bootstrap_abi::image;
use component_startup::{
    Requirements as _, SessionError, component_manifest, component_startup as startup_contract,
    serve_export,
};
use filesystem_abi::{file_system, file_system_mount, namespace};
use ipc::{Incoming, Listener, RecvError, wire::MESSAGE_INLINE_MAX};
use kernel_objects::{Delivery, Port};
use runtime::{FAILED_EXIT_CODE, PortTransport, exit, log_fmt, log_line, spawn_owned};
use syscall::{Handle, SyscallError, Timeout, kind};

mod router;
mod table;

use router::{Router, mount_gone, open_mount, open_view, view_gone};

component_manifest! {
    struct RouterStartup {
        requirements: {
            image: session(image::Protocol),
            filesystem: session(file_system::Protocol),
        },
        exports: [namespace::Protocol, file_system_mount::Protocol],
    }
}

#[derive(Debug)]
enum Stopped {
    Runtime(kernel_objects::Error),
    ServicePortClosed,
}

impl fmt::Display for Stopped {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Runtime(error) => error.fmt(formatter),
            Self::ServicePortClosed => formatter.write_str("service port closed"),
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
    let requirements = RouterStartup::from_parent(parent);
    let table = table::read(&requirements.image);
    let views: &'static Port = Box::leak(Box::new(Port::create()?));
    let mounts: &'static Port = Box::leak(Box::new(Port::create()?));

    // Потоки монтирования и экспортов работают до конца процесса. Поток монтирования при
    // монтировании отвечает на ожидающие запросы видов вызовом `open_root`, поэтому его стек
    // равен стеку основного потока.
    let views_parent = requirements.parent;
    let mount_parent = startup_contract::Client::new(PortTransport::new(parent));
    let _ = spawn_owned(move || {
        listen(mounts, mount_gone, |frame| {
            let _ = file_system_mount::dispatch(&mut Router { views }, frame);
        });
        log_line("mount port closed");
        exit(FAILED_EXIT_CODE)
    })?;
    let _ = spawn_owned(move || export::<namespace::Protocol>(&views_parent, views, open_view))?;
    let _ = spawn_owned(move || {
        export::<file_system_mount::Protocol>(&mount_parent, mounts, open_mount)
    })?;
    table::start(table, parent, views, mounts);

    listen(views, view_gone, |frame| {
        let _ = namespace::dispatch(&mut Router { views }, frame);
    });
    Err(Stopped::ServicePortClosed)
}

fn listen(port: &Port, gone: fn(u64), mut received: impl FnMut(Incoming<'_, Delivery>)) {
    let transport = PortTransport::new(port.borrow());
    let mut bytes = [0u8; MESSAGE_INLINE_MAX];
    loop {
        match transport.recv(&mut bytes, Timeout::INFINITE) {
            Ok(frame) => received(frame),
            Err(RecvError::SenderGone(badge)) => gone(badge),
            // Отброшенный кадр и пробуждение без кадра не закрывают порт.
            Err(RecvError::Discarded | RecvError::Timeout) => {}
            Err(RecvError::Closed) => return,
        }
    }
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
