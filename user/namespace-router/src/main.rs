//! Маршрутизатор имён: держит таблицу монтирования, поднимает процессы файловых систем из
//! userland-образа и выдаёт клиентам сессии с ними. Данные через него не идут.
//!
//! Одноразовый startup-порт передаёт сервисный и admin-порты. Сервисный порт разбирает пути
//! клиентов, admin-порт управляет таблицей монтирования и клиентам недоступен.
//! Admin-порт обслуживает отдельный поток. Монтирование не ждёт завершения клиентского
//! вызова сервисным потоком.

#![no_std]
#![no_main]
#![allow(unsafe_code)]

extern crate alloc;

use core::panic::PanicInfo;

use filesystem_abi::{
    NamespaceStartupFrame, dispatch_namespace, dispatch_namespace_admin, dispatch_namespace_startup,
};
use ipc::{Transport as _, wire::IpcError};
use runtime::{
    MemoryRegion, Mutex, OwnedHandle, PortTransport, Priority, ReadOnlyMapping, Resource,
    install_log, log_fmt, log_line, receive_startup, spawn_owned, thread_exit,
};
use spin::Once;
use syscall::{Handle, PORT_TIMEOUT_INFINITE, Rights};

mod error;
mod router;

use error::{Error, Result};
use router::{Administering, MountTable, Resolving};

const LOG_TAG: &str = "router";

// Стек потока admin-порта: он ведёт монтирования и подъём процессов файловых систем.
const ADMIN_STACK: u64 = 0x4000;

// Маппинг региона образа: его байты держат программы файловых систем и живут всё время работы
// маршрутизатора.
static IMAGE: Once<ReadOnlyMapping> = Once::new();

// Таблица монтирования, разделённая потоками сервисного и admin-порта.
static MOUNTS: Mutex<MountTable> = Mutex::new(MountTable::new());

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

// Принимает стартовый кадр, отдаёт admin-порт своему потоку и обслуживает сервисный порт, пока
// приём кадров не откажет.
fn run(startup: Handle) -> Result<()> {
    let StartResources {
        log,
        image,
        service,
        admin,
    } = handshake(startup)?;
    // Копия канала логирования для поднимаемых файловых систем: право минта остаётся за
    // маршрутизатором, детям уходит капа без него.
    let log_for_children =
        log.duplicate(Rights::WRITE | Rights::TRANSFER | Rights::DUPLICATE, 0)?;
    install_log(log, LOG_TAG);

    let image = MemoryRegion::from_handle(image);
    let mapping = image.map_full_read_only()?;
    let bytes = IMAGE.call_once(|| mapping).as_bytes();
    let admin_port = AdminPort {
        port: admin,
        administering: Administering::new(
            &MOUNTS,
            bytes,
            image,
            log_for_children,
            Resource::self_resource()?,
        ),
    };
    // Поток admin-порта живёт до конца процесса: join ему не нужен, а стек он держит своим.
    let _ = spawn_owned(
        move || serve_admin(admin_port),
        ADMIN_STACK,
        Priority::new(1),
    )?;
    log_line("started");

    serve(&service);
    Ok(())
}

// Цикл сервисного порта: кадры клиентов до его закрытия.
fn serve(service: &OwnedHandle) {
    let transport = PortTransport::server(service.borrow());
    loop {
        match transport.wait_readable(PORT_TIMEOUT_INFINITE) {
            Ok(()) => {}
            // Оба исхода кадра не несут: маршрутизатор не хранит сессий, а покадровое
            // исчерпание не закрывает порт.
            Err(IpcError::SenderGone | IpcError::ResourceExhausted) => continue,
            Err(IpcError::PeerClosed) => break,
            Err(_) => {
                log_line("service port failed");
                return;
            }
        }
        let mut resolving = Resolving::new(&MOUNTS);
        let delivered = dispatch_namespace(&mut resolving, &transport).is_ok();
        resolving.end_frame(delivered);
        // Reply-объект не переживает свой кадр: закрытие неотправленного ответа будит
        // припаркованного вызывателя.
        transport.cancel_reply();
    }
    log_line("service port closed");
}

// Состояние потока admin-порта.
struct AdminPort {
    port: OwnedHandle,
    administering: Administering<'static>,
}

// Цикл admin-порта в отдельном потоке: монтирования, пока порт открыт.
fn serve_admin(mut admin: AdminPort) -> u32 {
    let transport = PortTransport::server(admin.port.borrow());
    loop {
        match transport.wait_readable(PORT_TIMEOUT_INFINITE) {
            Ok(()) => {}
            // Оба исхода кадра не несут. Уход поднявшего оставляет монтирования работать,
            // а покадровое исчерпание не закрывает порт.
            Err(IpcError::SenderGone | IpcError::ResourceExhausted) => continue,
            Err(IpcError::PeerClosed) => break,
            // Прочий отказ приёма оставляет маршрутизатор без монтирований и уходит ненулевым
            // кодом потока, отличимым от закрытия порта.
            Err(_) => {
                log_line("admin port failed");
                return 1;
            }
        }
        let _ = dispatch_namespace_admin(&mut admin.administering, &transport);
        transport.cancel_reply();
    }
    log_line("admin port closed");
    0
}

// Capability стартового кадра.
struct StartResources {
    log: OwnedHandle,
    image: OwnedHandle,
    service: OwnedHandle,
    admin: OwnedHandle,
}

fn handshake(startup: Handle) -> Result<StartResources> {
    let NamespaceStartupFrame {
        log,
        image,
        service,
        admin,
    } = receive_startup(startup, dispatch_namespace_startup)?.ok_or(Error::Start)?;
    Ok(StartResources {
        log: OwnedHandle::adopt(log),
        image: OwnedHandle::adopt(image),
        service: OwnedHandle::adopt(service),
        admin: OwnedHandle::adopt(admin),
    })
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    log_fmt(format_args!("panic: {info}"));
    thread_exit(1)
}
