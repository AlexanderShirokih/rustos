//! Маршрутизатор имён разбирает пути клиентов `Namespace` по монтированиям и выдаёт сессии
//! файловых систем. Держатель `FileSystemMount` задаёт путь и файловую сессию монтирования.

#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

extern crate alloc;

use alloc::boxed::Box;
use core::{convert::Infallible, panic::PanicInfo};

use component_startup::{
    Requirements as _, SessionError, component_manifest, component_startup as startup_contract,
    serve_export,
};
use filesystem_abi::{file_system_mount, namespace};
use ipc::{Incoming, Listener, RecvError, wire::MESSAGE_INLINE_MAX};
use kernel_objects::{Delivery, Port};
use runtime::{
    PortTransport, Priority, log_fmt, log_line, process_exit, spawn_owned, start_component,
};
use syscall::{Handle, Timeout, kind};

mod error;
mod router;

use error::Result;
use router::{Router, mount_gone, open_mount, open_view, view_gone};

const LOG_TAG: &str = "router";

const MOUNT_STACK_SIZE: u64 = 0x10000;
const EXPORT_THREAD_STACK_SIZE: u64 = 0x10000;

component_manifest! {
    struct RouterStartup {
        requirements: {},
        exports: [namespace::Protocol, file_system_mount::Protocol],
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(parent: Handle<kind::Port>) -> ! {
    let Err(error) = run(parent);
    log_fmt(format_args!("stopped: {error}"));
    process_exit(1)
}

fn run(parent: Handle<kind::Port>) -> Result<Infallible> {
    start_component(parent, LOG_TAG);
    let requirements = RouterStartup::request(parent)?;
    let views: &'static Port = Box::leak(Box::new(Port::create()?));
    let mounts: &'static Port = Box::leak(Box::new(Port::create()?));

    // Потоки монтирования и экспортов работают до конца процесса. Поток монтирования при
    // монтировании отвечает на ожидающие запросы видов вызовом `open_root`, поэтому его стек
    // равен стеку основного потока.
    let views_parent = requirements.parent;
    let mount_parent = startup_contract::Client::new(PortTransport::new(parent));
    let _ = spawn_owned(
        move || {
            listen(mounts, mount_gone, |frame| {
                let _ = file_system_mount::dispatch(&mut Router { views }, frame);
            });
            log_line("mount port closed");
            process_exit(1)
        },
        MOUNT_STACK_SIZE,
        Priority::new(1),
    )?;
    let _ = spawn_owned(
        move || export::<namespace::Protocol>(&views_parent, views, open_view),
        EXPORT_THREAD_STACK_SIZE,
        Priority::new(1),
    )?;
    let _ = spawn_owned(
        move || export::<file_system_mount::Protocol>(&mount_parent, mounts, open_mount),
        EXPORT_THREAD_STACK_SIZE,
        Priority::new(1),
    )?;
    log_line("started");

    listen(views, view_gone, |frame| {
        let _ = namespace::dispatch(&mut Router { views }, frame);
    });
    log_line("service port closed");
    process_exit(1)
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
    open: fn(&Port) -> core::result::Result<Port, SessionError>,
) -> u32 {
    let failure = serve_export::<P>(parent, || open(port));
    log_fmt(format_args!("export stopped: {failure:?}"));
    process_exit(1)
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    log_fmt(format_args!("panic: {info}"));
    process_exit(1)
}
