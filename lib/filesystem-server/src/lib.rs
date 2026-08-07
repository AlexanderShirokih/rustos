//! Процесс файловой системы: стартовый кадр, сессии клиентов и цикл обслуживания. Крейт держит
//! всё, что у файловых систем общее: конкретная сводится к построению своего бекенда.
//!
//! Портов у процесса два. Сервисный порт создаёт он сам: минтить сессии вправе только тот, кто
//! отвечает за допуск к файловой системе. Admin-порт приносит `ProcessStart`, и на нём отвечает
//! отдельный поток: это отвязывает `bind` и стоящий за ним маршрутизатор имён от вызова,
//! который сервисный поток обслуживает прямо сейчас.
//!
//! Клиентская сторона admin-порта принадлежит поднявшему процесс маршрутизатору и несёт его
//! значок: её уход завершает процесс, а не оставляет монтирование сиротой.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use core::sync::atomic::AtomicU64;

use filesystem::FileSystemBackend;
use filesystem_abi::{dispatch_file_system, dispatch_file_system_admin};
use ipc::{Transport as _, wire::IpcError};
use runtime::{
    MemoryRegion, Mutex, OwnedHandle, Port, PortTransport, Priority, ReadOnlyMapping, Resource,
    Thread, exit_with_sibling, install_log, log_line, spawn_owned,
};
use spin::Once;
use syscall::{Handle, Timeout};

mod admin;
mod buffer;
mod error;
mod service;

use admin::{Binding, Handshake, StartResources};
pub use error::{Error, Result};
use service::{FileSystemState, Frame, Serving};
pub use service::{settle, vend};

const ADMIN_STACK: u64 = 0x4000;

const SUPERVISOR_LOST_EXIT_CODE: u32 = 2;

static IMAGE: Once<ReadOnlyMapping> = Once::new();

struct Shared {
    state: Mutex<FileSystemState>,
    service: OwnedHandle,
    admin: OwnedHandle,
    sessions_thread: Thread,
    next_badge: AtomicU64,
}

/// Поднимает файловую систему и обслуживает её, пока сервисный порт не закроется.
///
/// `admin` - серверная сторона admin-порта, полученная стартовым хэндлом процесса. `tag` метит
/// строки процесса в klog. `found` строит бекенд по байтам userland-образа. Файловой системе, не
/// опирающейся на образ, они не нужны.
pub fn serve<B>(
    admin: Handle,
    tag: &'static str,
    found: impl FnOnce(&'static [u8]) -> Result<B>,
) -> Result<()>
where
    B: FileSystemBackend + Send + 'static,
{
    let admin = OwnedHandle::from_handle(admin);
    let StartResources { log, image } = handshake(&admin)?;
    install_log(log, tag);

    let image = MemoryRegion::from_handle(image);
    let mapping = image.map_full_read_only()?;
    let bytes = IMAGE.call_once(|| mapping).as_bytes();
    let backend = found(bytes)?;

    let shared: &'static Shared = Box::leak(Box::new(Shared {
        state: Mutex::new(FileSystemState::new(
            Box::new(backend),
            image,
            Resource::self_resource()?,
        )),
        service: Port::create()?.into_handle(),
        admin,
        sessions_thread: Thread::self_thread()?,
        next_badge: AtomicU64::new(0),
    }));
    // Поток admin-порта живёт до конца процесса: join ему не нужен, а стек он держит своим.
    let _ = spawn_owned(move || serve_admin(shared), ADMIN_STACK, Priority::new(1))?;
    log_line("started");
    serve_sessions(shared);
    Ok(())
}

fn handshake(admin: &OwnedHandle) -> Result<StartResources> {
    let transport = PortTransport::server(admin.borrow());
    let mut received = Handshake::default();
    dispatch_file_system_admin(&mut received, &transport)?;
    received.take().ok_or(Error::Start)
}

fn serve_sessions(shared: &Shared) {
    let transport = PortTransport::server(shared.service.borrow());
    let mut frame = Frame::default();
    loop {
        match transport.wait_readable(Timeout::INFINITE) {
            Ok(()) => {}
            // Уход значка кадра не несёт: сессия ушедшей стороны убирается, а приём продолжается.
            Err(IpcError::SenderGone) => {
                shared.state.lock().remove_session(transport.peer_badge());
                continue;
            }
            // Исчерпание относится к одному кадру, а бессрочный приём срока не имеет:
            // буфер приёма в обоих случаях не содержит доставленного сообщения.
            Err(IpcError::ResourceExhausted | IpcError::Timeout) => continue,
            Err(IpcError::PeerClosed) => break,
            Err(_) => {
                log_line("service port failed");
                return;
            }
        }
        frame.begin(
            transport.peer_badge(),
            transport.staged_caps().into_iter().flatten(),
        );
        let mut serving = Serving {
            state: shared.state.lock(),
            frame: &mut frame,
        };
        let delivered = dispatch_file_system(&mut serving, &transport).is_ok();
        drop(serving);
        frame.end(delivered);
        // Reply-объект не переживает свой кадр: закрытие неотправленного ответа будит
        // припаркованного вызывателя.
        transport.cancel_reply();
    }
    log_line("service port closed");
}

fn serve_admin(shared: &Shared) -> u32 {
    let transport = PortTransport::server(shared.admin.borrow());
    loop {
        match transport.wait_readable(Timeout::INFINITE) {
            Ok(()) => {}
            // Отправитель на admin-порту один - поднявший монтирование
            // маршрутизатор, поэтому его уход снимает процесс целиком.
            Err(IpcError::SenderGone) => {
                log_line("supervisor gone");
                exit_with_sibling(&shared.sessions_thread, SUPERVISOR_LOST_EXIT_CODE)
            }
            // Покадровое исчерпание и пробуждение без кадра цикл не закрывают.
            Err(IpcError::ResourceExhausted | IpcError::Timeout) => continue,
            Err(IpcError::PeerClosed) => break,
            // Прочий отказ приёма оставляет процесс без установления сессий и уходит ненулевым
            // кодом потока, отличимым от закрытия порта.
            Err(_) => {
                log_line("admin port failed");
                return 1;
            }
        }
        let mut binding = Binding::new(shared);
        let delivered = dispatch_file_system_admin(&mut binding, &transport).is_ok();
        binding.end(delivered);
        transport.cancel_reply();
    }
    log_line("admin port closed");
    0
}
