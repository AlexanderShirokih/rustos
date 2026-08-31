//! Процесс файловой системы: компонентные порты, сессии клиентов и цикл обслуживания. Крейт
//! держит всё, что у файловых систем общее: конкретная сводится к построению своего бекенда.
//!
//! Портов у процесса два. Control-порт устанавливает сессии по `ComponentSessions::open_session`,
//! service-порт обслуживает их и выдаёт производные по `open_root`. Control-порт обслуживает
//! отдельный поток: это отвязывает rootkeeper от вызова, который сервисный поток обслуживает
//! прямо сейчас.
//!
//! Клиентская сторона control-порта принадлежит rootkeeper и несёт его значок: её уход завершает
//! процесс. Уход прочих значков этого порта обслуживание продолжает.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use core::sync::atomic::AtomicU64;

use component_abi::{component_sessions, supervisor_gone};
use filesystem::FileSystemBackend;
use filesystem_abi::file_system;
use ipc::{
    Transport as _,
    wire::{Cap, IpcError},
};
use kernel_objects::{MemoryRegion, OwnedHandle, ReadOnlyMapping, Resource, Thread};
use runtime::{
    Mutex, PortTransport, Priority, exit_with_sibling, install_log, log_line, spawn_owned,
};
use spin::Once;
use syscall::{Handle, Timeout};

mod admin;
mod buffer;
mod error;
mod service;
mod store;

use admin::Establishing;
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

/// Поднимает файловую систему и обслуживает её до закрытия service-порта.
///
/// `control` и `service` приходят компонентной стартовой транзакцией. `found` строит бекенд из
/// `Some(bytes)` отображённого региона либо из `None` для автономного бекенда.
pub fn serve<B>(
    log: Cap,
    control: Cap,
    service: Cap,
    backing: Option<Cap>,
    tag: &'static str,
    found: impl FnOnce(Option<&'static [u8]>) -> Result<B>,
) -> Result<()>
where
    B: FileSystemBackend + Send + 'static,
{
    install_log(Handle::from_nonzero(log.raw()), tag);
    let control = OwnedHandle::adopt(control);
    let service = OwnedHandle::adopt(service);
    let backing = backing.map(MemoryRegion::adopt);
    let bytes = match backing.as_ref() {
        Some(backing) => {
            let mapping = backing.map_full_read_only()?;
            Some(IMAGE.call_once(|| mapping).as_bytes())
        }
        None => None,
    };
    let backend = found(bytes)?;

    let shared: &'static Shared = Box::leak(Box::new(Shared {
        state: Mutex::new(FileSystemState::new(
            Box::new(backend),
            backing,
            Resource::self_resource()?,
        )),
        service,
        admin: control,
        sessions_thread: Thread::self_thread()?,
        next_badge: AtomicU64::new(0),
    }));
    // Control-поток разделяет время жизни процесса и владеет собственным стеком.
    let _ = spawn_owned(move || serve_admin(shared), ADMIN_STACK, Priority::new(1))?;
    log_line("started");
    serve_sessions(shared);
    Ok(())
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
            service: &shared.service,
            next_badge: &shared.next_badge,
        };
        let delivered = file_system::dispatch(&mut serving, &transport).is_ok();
        drop(serving);
        frame.end(delivered, &mut shared.state.lock());
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
            // Уход значка rootkeeper на control-порту снимает процесс целиком, а уход
            // прочих значков порта цикл продолжает.
            Err(IpcError::SenderGone) if supervisor_gone(transport.peer_badge()) => {
                log_line("supervisor gone");
                exit_with_sibling(
                    shared.sessions_thread.handle().as_raw(),
                    SUPERVISOR_LOST_EXIT_CODE,
                )
            }
            // Покадровое исчерпание, пробуждение без кадра и уход чужого значка цикл не
            // закрывают.
            Err(IpcError::SenderGone | IpcError::ResourceExhausted | IpcError::Timeout) => continue,
            Err(IpcError::PeerClosed) => break,
            // Прочий отказ приёма оставляет процесс без установления сессий и уходит ненулевым
            // кодом потока, отличимым от закрытия порта.
            Err(_) => {
                log_line("control port failed");
                return 1;
            }
        }
        let mut establishing = Establishing::new(shared);
        let delivered = component_sessions::dispatch(&mut establishing, &transport).is_ok();
        establishing.end(delivered);
        transport.cancel_reply();
    }
    log_line("control port closed");
    0
}
