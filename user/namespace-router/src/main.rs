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

use component_abi::{ComponentConfig, component_manifest};
use filesystem_abi::{decode_mount_table, dispatch_namespace, dispatch_namespace_admin};
use ipc::{Transport as _, wire::IpcError};
use runtime::{
    MemoryRegion, Mutex, OwnedHandle, PortTransport, Priority, ReadOnlyMapping, Resource, Thread,
    exit_with_sibling, install_log, log_fmt, log_line, spawn_owned, thread_exit,
};
use spin::Once;
use syscall::{Handle, Rights, Timeout};
use userland_image::decode;

mod error;
mod router;

use error::{Error, Result};
use router::{Administering, MountTable, Resolving};

const LOG_TAG: &str = "router";

// Стек потока admin-порта: он ведёт монтирования и подъём процессов файловых систем.
const ADMIN_STACK: u64 = 0x4000;

// Код выхода маршрутизатора, потерявшего поднявший его supervisor.
const SUPERVISOR_LOST_EXIT_CODE: u32 = 7;

// Оборот приёма на admin-порту: он же задаёт задержку между уходом файловой системы и подъёмом
// её смены.
const ADMIN_TURN: Timeout = Timeout::from_millis(100);

// Маппинг региона образа: его байты держат программы файловых систем и живут всё время работы
// маршрутизатора.
static IMAGE: Once<ReadOnlyMapping> = Once::new();

// Таблица монтирования, разделённая потоками сервисного и admin-порта.
static MOUNTS: Mutex<MountTable> = Mutex::new(MountTable::new());

component_manifest! {
    struct RouterStartup {
        config,
        slots: { image: image },
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

// Принимает стартовый кадр, отдаёт admin-порт своему потоку и обслуживает сервисный порт, пока
// приём кадров не откажет.
fn run(startup: Handle) -> Result<()> {
    let StartResources {
        log,
        image,
        service,
        control,
        mounts,
    } = handshake(startup)?;
    // Копия канала логирования для поднимаемых файловых систем: право минта остаётся за
    // маршрутизатором, детям уходит капа без него.
    let log_for_children =
        log.duplicate(Rights::WRITE | Rights::TRANSFER | Rights::DUPLICATE, None)?;
    install_log(log, LOG_TAG);

    let image = MemoryRegion::from_handle(image);
    let mapping = image.map_full_read_only()?;
    let bytes = IMAGE.call_once(|| mapping).as_bytes();
    let admin_port = AdminPort {
        port: control,
        administering: Administering::new(
            &MOUNTS,
            bytes,
            image,
            log_for_children,
            Resource::self_resource()?,
        ),
        service_thread: Thread::self_thread()?,
    };
    mount_boot_table(&admin_port.administering, bytes, mounts.as_str())?;
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
        match transport.wait_readable(Timeout::INFINITE) {
            Ok(()) => {}
            // Ни один исход кадра не несёт: маршрутизатор не хранит сессий, а покадровое
            // исчерпание и пробуждение без кадра порт не закрывают.
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
    service_thread: Thread,
}

// Цикл admin-порта в отдельном потоке: монтирования и наблюдение за их процессами, пока порт
// открыт. Наблюдение идёт каждым оборотом, а не только пустым: поток admin-кадров его не
// вытесняет.
fn serve_admin(mut admin: AdminPort) -> u32 {
    let transport = PortTransport::server(admin.port.borrow());
    loop {
        match transport.wait_readable(ADMIN_TURN) {
            Ok(()) => {
                let _ = dispatch_namespace_admin(&mut admin.administering, &transport);
                transport.cancel_reply();
            }
            Err(IpcError::Timeout) => {}
            // Отправитель на admin-порту один - поднявший маршрутизатор supervisor, поэтому
            // его уход снимает процесс целиком вместе с монтированиями.
            Err(IpcError::SenderGone) => {
                log_line("supervisor gone");
                exit_with_sibling(&admin.service_thread, SUPERVISOR_LOST_EXIT_CODE)
            }
            // Покадровое исчерпание не закрывает порт.
            Err(IpcError::ResourceExhausted) => continue,
            Err(IpcError::PeerClosed) => break,
            // Прочий отказ приёма оставляет маршрутизатор без монтирований и уходит ненулевым
            // кодом потока, отличимым от закрытия порта.
            Err(_) => {
                log_line("admin port failed");
                return 1;
            }
        }
        admin.administering.supervise();
    }
    log_line("admin port closed");
    0
}

fn mount_boot_table(
    administering: &router::Administering<'_>,
    image_bytes: &[u8],
    mounts: &str,
) -> Result<()> {
    let record = decode(image_bytes)?
        .entry(mounts)
        .ok_or(Error::MountTable)?;
    let table = decode_mount_table(record.data()).map_err(|error| {
        log_fmt(format_args!("boot mount table rejected: {error:?}"));
        Error::MountTable
    })?;
    administering.mount_boot_table(table)
}

struct StartResources {
    log: OwnedHandle,
    image: OwnedHandle,
    service: OwnedHandle,
    control: OwnedHandle,
    mounts: ComponentConfig,
}

fn handshake(startup: Handle) -> Result<StartResources> {
    let startup = OwnedHandle::from_handle(startup);
    let started =
        RouterStartup::receive(&PortTransport::server(startup.borrow()))?.ok_or(Error::Start)?;
    Ok(StartResources {
        log: OwnedHandle::adopt(started.log),
        image: OwnedHandle::adopt(started.image),
        service: OwnedHandle::adopt(started.service),
        control: OwnedHandle::adopt(started.control),
        mounts: started.config,
    })
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    log_fmt(format_args!("panic: {info}"));
    thread_exit(1)
}
