//! Маршрутизатор имён разбирает пути клиентов `Namespace` по монтированиям и выдаёт сессии
//! файловых систем. Файловая система сама монтирует себя по своей сессии `NamespaceAdmin`.

#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

extern crate alloc;

use alloc::boxed::Box;
use core::{convert::Infallible, panic::PanicInfo};

use component_startup::{
    SessionError, component_manifest, component_startup as startup_contract, serve_export,
};
use filesystem_abi::{namespace, namespace_admin};
use ipc::{Incoming, Listener, RecvError, wire::MESSAGE_INLINE_MAX};
use kernel_objects::{Delivery, Port};
use runtime::{PortTransport, Priority, install_log, log_fmt, log_line, process_exit, spawn_owned};
use syscall::{Handle, Timeout, kind};

mod error;
mod router;

use error::Result;
use router::{Router, admin_gone, open_admin, open_view, view_gone};

const LOG_TAG: &str = "router";

const ADMIN_STACK: u64 = 0x10000;
const EXPORT_STACK: u64 = 0x4000;

const PANIC_EXIT_CODE: u64 = 1;
const SERVICE_PORT_FAILED_EXIT_CODE: u64 = 9;
const ADMIN_PORT_FAILED_EXIT_CODE: u64 = 10;
const EXPORT_FAILED_EXIT_CODE: u64 = 11;

component_manifest! {
    struct RouterStartup {
        requirements: {},
        exports: [namespace::Protocol, namespace_admin::Protocol],
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(parent: Handle<kind::Port>) -> ! {
    let Err(error) = run(parent);
    log_fmt(format_args!("stopped: {error}"));
    process_exit(u64::from(error.exit_code()))
}

fn run(parent: Handle<kind::Port>) -> Result<Infallible> {
    let started = RouterStartup::start(parent)?;
    if let Some(log) = started.log {
        install_log(log.into_handle().into_raw(), LOG_TAG);
    }
    let views: &'static Port = Box::leak(Box::new(Port::create()?));
    let admins: &'static Port = Box::leak(Box::new(Port::create()?));

    // Потоки администратора и экспортов работают до конца процесса. Поток администратора при
    // монтировании отвечает на ожидающие запросы видов вызовом `open_root`, поэтому его стек
    // равен стеку основного потока.
    let views_parent = started.requirements.parent;
    let admins_parent = startup_contract::Client::new(PortTransport::new(parent));
    let _ = spawn_owned(
        move || {
            listen(admins, admin_gone, |frame| {
                let _ = namespace_admin::dispatch(&mut Router, frame);
            });
            log_line("admin port closed");
            process_exit(ADMIN_PORT_FAILED_EXIT_CODE)
        },
        ADMIN_STACK,
        Priority::new(1),
    )?;
    let _ = spawn_owned(
        move || export::<namespace::Protocol>(&views_parent, views, open_view),
        EXPORT_STACK,
        Priority::new(1),
    )?;
    let _ = spawn_owned(
        move || export::<namespace_admin::Protocol>(&admins_parent, admins, open_admin),
        EXPORT_STACK,
        Priority::new(1),
    )?;
    log_line("started");

    listen(views, view_gone, |frame| {
        let _ = namespace::dispatch(&mut Router, frame);
    });
    log_line("service port closed");
    process_exit(SERVICE_PORT_FAILED_EXIT_CODE)
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
    open: fn(&Port, &str) -> core::result::Result<Port, SessionError>,
) -> u32 {
    let failure = serve_export::<P>(parent, |scope| open(port, scope));
    log_fmt(format_args!("export stopped: {failure:?}"));
    process_exit(EXPORT_FAILED_EXIT_CODE)
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    log_fmt(format_args!("panic: {info}"));
    process_exit(PANIC_EXIT_CODE)
}
