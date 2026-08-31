//! Маршрутизатор имён владеет таблицей монтирования: под префиксом живёт сессия обслуживающей
//! его файловой системы, а клиентам он выдаёт окна на запрошенные пути. Данные клиенты передают
//! напрямую файловым системам.
//!
//! Одноразовый startup-порт передаёт service- и control-порты. Service-порт разбирает пути
//! клиентов, а control-порт обслуживает `NamespaceAdmin` для тех, чей слот назвал этот протокол.
//! Control-порт обслуживает отдельный поток.

#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

extern crate alloc;

use core::panic::PanicInfo;

use component_abi::{component_manifest, supervisor_gone};
use filesystem_abi::{namespace, namespace_admin};
use ipc::{Transport as _, wire::IpcError};
use kernel_objects::{OwnedHandle, Thread};
use runtime::{
    Mutex, PortTransport, Priority, exit_with_sibling, install_log, log_fmt, log_line, spawn_owned,
    thread_exit,
};
use syscall::{Handle, Timeout};

mod error;
mod router;

use error::{Error, Result};
use router::{Administering, MountTable, Resolving};

const LOG_TAG: &str = "router";

const ADMIN_STACK: u64 = 0x4000;

const SUPERVISOR_LOST_EXIT_CODE: u32 = 7;

const SERVICE_PORT_FAILED_EXIT_CODE: u32 = 9;

static MOUNTS: Mutex<MountTable> = Mutex::new(MountTable::new());

component_manifest! {
    struct RouterStartup {
        exports: "Namespace",
        controls: "NamespaceAdmin",
        slots: {},
    }
}

/// `startup` приходит в x0 как HandleId одноразового startup-порта.
#[unsafe(no_mangle)]
pub extern "C" fn _start(startup: Handle) -> ! {
    match run(startup) {
        Ok(()) => thread_exit(0),
        Err(error) => {
            log_fmt(format_args!("stopped: {error}"));
            thread_exit(u64::from(error.exit_code()))
        }
    }
}

fn run(startup: Handle) -> Result<()> {
    let StartResources {
        log,
        service,
        control,
    } = handshake(startup)?;
    install_log(log.into_raw(), LOG_TAG);

    let admin_port = AdminPort {
        port: control,
        administering: Administering::new(&MOUNTS),
        service_thread: Thread::self_thread()?,
    };
    // Control-поток разделяет время жизни процесса и владеет собственным стеком.
    let admin = spawn_owned(
        move || serve_admin(admin_port),
        ADMIN_STACK,
        Priority::new(1),
    )?;
    log_line("started");

    serve(&service);
    // Разбирать пути больше некому, и уход одного потока процесс не снимает:
    // supervisor узнаёт об уходе компонента только по его коду выхода.
    exit_with_sibling(admin.handle(), SERVICE_PORT_FAILED_EXIT_CODE)
}

fn serve(service: &OwnedHandle) {
    let transport = PortTransport::server(service.borrow());
    loop {
        match transport.wait_readable(Timeout::INFINITE) {
            Ok(()) => {}
            // Ни один исход кадра не несёт: покадровое исчерпание и пробуждение без кадра
            // порт не закрывают.
            Err(IpcError::SenderGone | IpcError::ResourceExhausted | IpcError::Timeout) => {
                continue;
            }
            Err(IpcError::PeerClosed) => break,
            Err(_) => {
                log_line("service port failed");
                return;
            }
        }
        let mut resolving = Resolving::new(&MOUNTS);
        let delivered = namespace::dispatch(&mut resolving, &transport).is_ok();
        resolving.end_frame(delivered);
        // Reply-объект не переживает свой кадр: закрытие неотправленного ответа будит
        // припаркованного вызывателя.
        transport.cancel_reply();
    }
    log_line("service port closed");
}

struct AdminPort {
    port: OwnedHandle,
    administering: Administering<'static>,
    service_thread: Thread,
}

fn serve_admin(mut admin: AdminPort) -> u32 {
    let transport = PortTransport::server(admin.port.borrow());
    loop {
        match transport.wait_readable(Timeout::INFINITE) {
            Ok(()) => {
                admin
                    .administering
                    .begin_frame(transport.staged_caps().into_iter().flatten());
                let _ = namespace_admin::dispatch(&mut admin.administering, &transport);
                admin.administering.end_frame();
                transport.cancel_reply();
            }
            // Уход значка поднявшего маршрутизатор supervisor снимает процесс целиком вместе
            // с монтированиями.
            Err(IpcError::SenderGone) if supervisor_gone(transport.peer_badge()) => {
                log_line("supervisor gone");
                exit_with_sibling(
                    admin.service_thread.handle().as_raw(),
                    SUPERVISOR_LOST_EXIT_CODE,
                )
            }
            // Покадровое исчерпание и уход значка потребителя `NamespaceAdmin` порт не
            // закрывают.
            Err(IpcError::SenderGone | IpcError::ResourceExhausted) => {}
            Err(IpcError::PeerClosed) => break,
            // Прочий отказ приёма оставляет маршрутизатор без управления таблицей и уходит
            // ненулевым кодом потока, отличимым от закрытия порта.
            Err(_) => {
                log_line("control port failed");
                return 1;
            }
        }
    }
    log_line("control port closed");
    0
}

struct StartResources {
    log: OwnedHandle,
    service: OwnedHandle,
    control: OwnedHandle,
}

fn handshake(startup: Handle) -> Result<StartResources> {
    let startup = OwnedHandle::from_handle(startup);
    let started =
        RouterStartup::receive(&PortTransport::server(startup.borrow()))?.ok_or(Error::Start)?;
    Ok(StartResources {
        log: OwnedHandle::adopt(started.log),
        service: OwnedHandle::adopt(started.service),
        control: OwnedHandle::adopt(started.control),
    })
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    log_fmt(format_args!("panic: {info}"));
    thread_exit(1)
}
