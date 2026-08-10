//! Маршрутизатор имён владеет таблицей монтирования, получает у supervisor сессии поднятых
//! файловых систем и выдаёт клиентам окна на запрошенные пути. Данные клиенты передают напрямую
//! файловым системам.
//!
//! Одноразовый startup-порт передаёт service- и control-порты. Service-порт разбирает пути
//! клиентов, а control-порт принимает операции над таблицей от capability rootkeeper.
//! Control-порт обслуживает отдельный поток.

#![no_std]
#![no_main]
#![allow(unsafe_code)]

extern crate alloc;

use core::panic::PanicInfo;

use component_abi::component_manifest;
use filesystem_abi::{BOOT_MOUNT_TABLE_PATH, decode_mount_table, namespace, namespace_admin};
use ipc::{Transport as _, wire::IpcError};
use runtime::{
    MemoryRegion, Mutex, OwnedHandle, PortTransport, Priority, ReadOnlyMapping, Thread,
    exit_with_sibling, install_log, log_fmt, log_line, spawn_owned, thread_exit,
};
use spin::Once;
use syscall::{Handle, Timeout};
use userland_image::decode;

mod error;
mod router;

use error::{Error, Result};
use router::{Administering, MountTable, Resolving};

const LOG_TAG: &str = "router";

const ADMIN_STACK: u64 = 0x4000;

const SUPERVISOR_LOST_EXIT_CODE: u32 = 7;

const SERVICE_PORT_FAILED_EXIT_CODE: u32 = 9;

static MOUNTS: Mutex<MountTable> = Mutex::new(MountTable::new());

static IMAGE: Once<ReadOnlyMapping> = Once::new();

component_manifest! {
    struct RouterStartup {
        exports: "Namespace",
        slots: { image: userland, supervisor: supervisor },
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
        image,
        supervisor,
        service,
        control,
    } = handshake(startup)?;
    install_log(log, LOG_TAG);

    let image = MemoryRegion::from_handle(image);
    let mapping = image.map_full_read_only()?;
    let bytes = IMAGE.call_once(|| mapping).as_bytes();

    let mut administering = Administering::new(&MOUNTS, supervisor);
    mount_boot_table(&mut administering, bytes)?;

    let admin_port = AdminPort {
        port: control,
        administering,
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
    exit_with_sibling(admin.thread(), SERVICE_PORT_FAILED_EXIT_CODE)
}

fn mount_boot_table(administering: &mut Administering<'_>, image: &[u8]) -> Result<()> {
    let record = decode(image)
        .ok()
        .and_then(|entries| entries.entry(BOOT_MOUNT_TABLE_PATH))
        .ok_or(Error::MountTable)?;
    let table = decode_mount_table(record.data()).map_err(|error| {
        log_fmt(format_args!("boot mount table rejected: {error:?}"));
        Error::MountTable
    })?;
    administering.mount_boot_table(table)
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
                let _ = namespace_admin::dispatch(&mut admin.administering, &transport);
                transport.cancel_reply();
            }
            // Отправитель на control-порту один - поднявший маршрутизатор supervisor, поэтому
            // его уход снимает процесс целиком вместе с монтированиями.
            Err(IpcError::SenderGone) => {
                log_line("supervisor gone");
                exit_with_sibling(&admin.service_thread, SUPERVISOR_LOST_EXIT_CODE)
            }
            // Покадровое исчерпание не закрывает порт.
            Err(IpcError::ResourceExhausted) => {}
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
    image: OwnedHandle,
    supervisor: OwnedHandle,
    service: OwnedHandle,
    control: OwnedHandle,
}

fn handshake(startup: Handle) -> Result<StartResources> {
    let startup = OwnedHandle::from_handle(startup);
    let started =
        RouterStartup::receive(&PortTransport::server(startup.borrow()))?.ok_or(Error::Start)?;
    Ok(StartResources {
        log: OwnedHandle::adopt(started.log),
        image: OwnedHandle::adopt(started.image),
        supervisor: OwnedHandle::adopt(started.supervisor),
        service: OwnedHandle::adopt(started.service),
        control: OwnedHandle::adopt(started.control),
    })
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    log_fmt(format_args!("panic: {info}"));
    thread_exit(1)
}
