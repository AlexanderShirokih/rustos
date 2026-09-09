//! Маршрутизатор имён владеет таблицей монтирования. Под префиксом в ней хранится сессия
//! обслуживающей его файловой системы, а клиентам он выдаёт окна на запрошенные пути. Данные
//! клиенты передают напрямую файловым системам.
//!
//! Startup-порт остаётся на всю incarnation. Экспорт `service` разбирает пути клиентов,
//! экспорт `admin` обслуживает `NamespaceAdmin` отдельным потоком.

#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

extern crate alloc;

use core::panic::PanicInfo;

use component_startup::{ReceivedStartup, component_manifest};
use filesystem_abi::{namespace, namespace_admin};
use ipc::{Transport as _, wire::IpcError};
use kernel_objects::{Port, ReceivedFrame};
use runtime::{
    Mutex, PortTransport, Priority, exit_with_sibling, install_log, log_fmt, log_line, spawn_owned,
    thread_exit,
};
use syscall::{Handle, Timeout, kind};

mod error;
mod router;

use error::Result;
use router::{Administering, MountTable, Resolving};

const LOG_TAG: &str = "router";

const ADMIN_STACK: u64 = 0x4000;

const SERVICE_PORT_FAILED_EXIT_CODE: u32 = 9;

static MOUNTS: Mutex<MountTable> = Mutex::new(MountTable::new());

component_manifest! {
    struct RouterStartup {
        exports: { service: "Namespace", admin: "NamespaceAdmin" },
        slots: {},
    }
}

/// `startup` - серверная сторона startup-порта, аргумент точки входа.
#[unsafe(no_mangle)]
pub extern "C" fn _start(startup: Handle<kind::Port>) -> ! {
    match run(startup) {
        Ok(()) => thread_exit(0),
        Err(error) => {
            log_fmt(format_args!("stopped: {error}"));
            thread_exit(u64::from(error.exit_code()))
        }
    }
}

fn run(startup: Handle<kind::Port>) -> Result<()> {
    let ReceivedStartup { log, resources } = RouterStartup::receive(Port::from_handle(startup))?;
    install_log(log.into_raw(), LOG_TAG);
    let RouterStartup {
        startup,
        service,
        admin,
    } = resources;

    // Startup-порт открыт всю incarnation, и по нему supervisor запрашивает сессии экспортов.
    let _startup = startup;
    // Admin-поток разделяет время жизни процесса и владеет собственным стеком.
    let admin = spawn_owned(move || serve_admin(&admin), ADMIN_STACK, Priority::new(1))?;
    log_line("started");

    serve(&service);
    // Разбирать пути больше некому, и уход одного потока процесс не снимает:
    // supervisor узнаёт об уходе компонента только по его коду выхода.
    exit_with_sibling(admin.handle(), SERVICE_PORT_FAILED_EXIT_CODE)
}

fn serve(service: &Port) {
    let transport = PortTransport::server(service.borrow());
    loop {
        match transport.wait_readable(Timeout::INFINITE) {
            Ok(()) => {}
            // Уход значка, покадровое исчерпание и пробуждение без кадра порт не закрывают.
            Err(IpcError::SenderGone | IpcError::ResourceExhausted | IpcError::Timeout) => {
                continue;
            }
            Err(IpcError::PeerClosed) => break,
            Err(_) => {
                log_line("service port failed");
                return;
            }
        }
        // Приём выше доставил кадр, и его незавершённый ответ принадлежит владеющему кадру
        // до конца обработки.
        let received = ReceivedFrame::take(&transport);
        let mut resolving = Resolving::new(&MOUNTS);
        let delivered = namespace::dispatch(&mut resolving, &transport).is_ok();
        resolving.end_frame(delivered);
        drop(received);
    }
    log_line("service port closed");
}

fn serve_admin(port: &Port) -> u32 {
    let transport = PortTransport::server(port.borrow());
    loop {
        match transport.wait_readable(Timeout::INFINITE) {
            // Приём выше доставил кадр, и его capability вместе с незавершённым ответом
            // принадлежат владеющему кадру до конца обработки.
            Ok(()) => {
                let Some(mut received) = ReceivedFrame::take(&transport) else {
                    continue;
                };
                let mut administering = Administering::new(&MOUNTS, &mut received);
                let _ = namespace_admin::dispatch(&mut administering, &transport);
            }
            // Покадровое исчерпание и уход значка потребителя `NamespaceAdmin` порт не
            // закрывают.
            Err(IpcError::SenderGone | IpcError::ResourceExhausted) => {}
            Err(IpcError::PeerClosed) => break,
            // Прочий отказ приёма оставляет маршрутизатор без управления таблицей и уходит
            // ненулевым кодом потока, отличимым от закрытия порта.
            Err(_) => {
                log_line("admin port failed");
                return 1;
            }
        }
    }
    log_line("admin port closed");
    0
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    log_fmt(format_args!("panic: {info}"));
    thread_exit(1)
}
