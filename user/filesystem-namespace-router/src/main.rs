//! Маршрутизатор имён: держит таблицу монтирования, поднимает процессы файловых систем из
//! userland-образа и выдаёт клиентам сессии с ними. Данные через него не идут.
//!
//! Портов у процесса два. Сервисный порт разбирает пути клиентов, admin-порт несёт стартовый
//! кадр и управление таблицей монтирования - он отдан тому, кто маршрутизатор поднял, и клиентам
//! недоступен. Admin-порт обслуживает отдельный поток: монтирование не ждёт, пока сервисный
//! поток закончит вызов клиента.

#![no_std]
#![no_main]
#![allow(unsafe_code)]

extern crate alloc;

use alloc::boxed::Box;
use core::panic::PanicInfo;

use filesystem_abi::{
    FileSystemError, NamespaceAdminService, PATH_MAX, dispatch_namespace, dispatch_namespace_admin,
};
use ipc::{
    Transport as _,
    wire::{Cap, IpcError, Str},
};
use runtime::{
    MemoryRegion, Mutex, OwnedHandle, PortTransport, Priority, ReadOnlyMapping, Resource,
    install_log, log_fmt, log_line, spawn, thread_exit,
};
use spin::Once;
use syscall::{Handle, PORT_TIMEOUT_INFINITE, Rights};

mod error;
mod router;

use error::{Error, Result};
use router::{Administering, MountTable, Resolving};

// Тег строк маршрутизатора в klog.
const LOG_TAG: &str = "filesystem-namespace-router";

// Стек потока admin-порта: он ведёт монтирования и подъём процессов файловых систем.
const ADMIN_STACK: u64 = 0x4000;

// Маппинг региона образа: его байты держат программы файловых систем и живут всё время работы
// маршрутизатора.
static IMAGE: Once<ReadOnlyMapping> = Once::new();

// Таблица монтирования, разделённая потоками сервисного и admin-порта.
static MOUNTS: Mutex<MountTable> = Mutex::new(MountTable::new());

/// `admin` приходит в x0 как HandleId серверной стороны admin-порта.
#[unsafe(no_mangle)]
pub extern "C" fn _start(admin: Handle) -> ! {
    match run(admin) {
        Ok(()) => thread_exit(0),
        Err(error) => {
            log_fmt(format_args!("stopped: {error}"));
            thread_exit(1)
        }
    }
}

// Принимает стартовый кадр, отдаёт admin-порт своему потоку и обслуживает сервисный порт, пока
// приём кадров не откажет.
fn run(admin: Handle) -> Result<()> {
    let admin = OwnedHandle::from_handle(admin);
    let StartResources {
        log,
        image,
        service,
    } = handshake(&admin)?;
    // Копия канала логирования для поднимаемых файловых систем: право минта остаётся за
    // маршрутизатором, детям уходит капа без него.
    let log_for_children =
        log.duplicate(Rights::WRITE | Rights::TRANSFER | Rights::DUPLICATE, 0)?;
    install_log(log, LOG_TAG);

    let image = MemoryRegion::from_handle(image);
    let mapping = image.map_full_read_only()?;
    let bytes = IMAGE.call_once(|| mapping).as_bytes();
    // Поток admin-порта живёт до конца процесса: join ему не нужен, а стек он держит своим.
    let admin_port = Box::leak(Box::new(AdminPort {
        port: admin,
        administering: Administering::new(
            &MOUNTS,
            bytes,
            image,
            log_for_children,
            Resource::self_resource()?,
        ),
    }));
    let _ = spawn(
        serve_admin,
        core::ptr::from_mut::<AdminPort>(admin_port) as usize,
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
            // Уход значка кадра не несёт: сессий у маршрутизатора нет, приём продолжается.
            Err(IpcError::SenderGone) => continue,
            // Кадр доставлен, но принять его нечем: неудача касается одного кадра, порт жив.
            Err(IpcError::ResourceExhausted) => {
                log_line("frame dropped: resources exhausted");
                continue;
            }
            Err(_) => break,
        }
        let mut resolving = Resolving::new(&MOUNTS);
        let dispatched = dispatch_namespace(&mut resolving, &transport);
        resolving.end_frame(dispatched.is_ok());
        // Reply-объект не переживает свой кадр: закрытие неотправленного ответа будит
        // припаркованного вызывателя.
        transport.cancel_reply();
        if dispatched.is_err() {
            log_line("frame rejected");
        }
    }
    log_line("service port closed");
}

// Аргумент потока admin-порта: сам порт и admin-сторона с ресурсами подъёма.
struct AdminPort {
    port: OwnedHandle,
    administering: Administering<'static>,
}

/// Цикл admin-порта в отдельном потоке: монтирования, пока порт открыт.
extern "C" fn serve_admin(arg: usize) -> u32 {
    // SAFETY: `arg` - указатель на `AdminPort`, утёкший в `run`: он живёт до конца процесса, и
    // этот поток - единственный его владелец.
    let admin: &mut AdminPort = unsafe { &mut *(arg as *mut AdminPort) };
    let transport = PortTransport::server(admin.port.borrow());
    loop {
        match transport.wait_readable(PORT_TIMEOUT_INFINITE) {
            Ok(()) => {}
            // Уход поднявшего новых монтирований не несёт, а выданные продолжают работать.
            Err(IpcError::SenderGone) => continue,
            // Кадр доставлен, но принять его нечем: неудача касается одного кадра, порт жив.
            Err(IpcError::ResourceExhausted) => {
                log_line("admin frame dropped: resources exhausted");
                continue;
            }
            Err(IpcError::PeerClosed) => break,
            // Прочий отказ приёма оставляет маршрутизатор без монтирований и уходит ненулевым
            // кодом потока, отличимым от закрытия порта.
            Err(_) => {
                log_line("admin port failed");
                return 1;
            }
        }
        let dispatched = dispatch_namespace_admin(&mut admin.administering, &transport);
        transport.cancel_reply();
        if dispatched.is_err() {
            log_line("admin frame rejected");
        }
    }
    log_line("admin port closed");
    0
}

// Capability стартового кадра.
struct StartResources {
    log: OwnedHandle,
    image: OwnedHandle,
    service: OwnedHandle,
}

// Сторона admin-порта до стартового кадра: монтирований у неё ещё нет.
#[derive(Default)]
struct Handshake {
    resources: Option<StartResources>,
}

impl NamespaceAdminService for Handshake {
    fn start(&mut self, log: Cap, image: Cap, service: Cap) {
        self.resources = Some(StartResources {
            log: OwnedHandle::adopt(log),
            image: OwnedHandle::adopt(image),
            service: OwnedHandle::adopt(service),
        });
    }

    /// Монтирования заводит поднятый маршрутизатор: до стартового кадра дерева имён ещё нет.
    fn mount(
        &mut self,
        _path: Str<'_, PATH_MAX>,
        _program: Str<'_, PATH_MAX>,
    ) -> core::result::Result<u32, FileSystemError> {
        Err(FileSystemError::NoSession)
    }

    fn unmount(&mut self, _path: Str<'_, PATH_MAX>) -> core::result::Result<u32, FileSystemError> {
        Err(FileSystemError::NoSession)
    }
}

// Кадр без capability - Start.
fn handshake(admin: &OwnedHandle) -> Result<StartResources> {
    let transport = PortTransport::server(admin.borrow());
    let mut received = Handshake::default();
    dispatch_namespace_admin(&mut received, &transport)?;
    received.resources.ok_or(Error::Start)
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    log_fmt(format_args!("panic: {info}"));
    thread_exit(1)
}
