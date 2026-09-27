//! Процесс файловой системы: сессии экспорта `FileSystem` и монтирование себя в дерево имён по
//! сессии администратора. После ухода маршрутизатора сервер монтирует себя снова.
//!
//! Крейт содержит общую часть файловых систем, а конкретная файловая система строит только
//! свой бекенд.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use core::{convert::Infallible, sync::atomic::AtomicU64};

use filesystem::FileSystemBackend;
use filesystem_abi::{file_system, namespace_admin};
use ipc::{
    Listener, RecvError,
    wire::{IpcError, MESSAGE_INLINE_MAX},
};
use kernel_objects::{MemoryRegion, Port, ReadOnlyMapping, Resource, Session};
use log_abi::log;
use runtime::{Mutex, PortTransport, Priority, install_log, log_line, spawn_owned};
use spin::Once;
use syscall::{Handle, Timeout, kind};

mod buffer;
mod error;
mod export;
mod mount;
mod service;
mod store;

pub use error::{Error, Result};
use service::{FileSystemState, Frame, Serving};

const EXPORT_STACK_SIZE: u64 = 0x4000;

const MOUNT_STACK_SIZE: u64 = 0x8000;

const FAILED_EXIT_CODE: u64 = 1;

static IMAGE: Once<ReadOnlyMapping> = Once::new();

struct Shared {
    state: Mutex<FileSystemState>,
    service: Port,
    next_badge: AtomicU64,
}

/// Поднимает файловую систему, монтирует её под областью сессии `namespace` и обслуживает
/// экспорт `FileSystem` до закрытия его порта. `found` строит бекенд из байт региона `backing`
/// либо автономный из `None`. Отказ монтирования завершает процесс кодом 1.
pub fn serve<B>(
    log: Option<Session<log::Protocol>>,
    start: Handle<kind::Port>,
    namespace: Session<namespace_admin::Protocol>,
    backing: Option<MemoryRegion>,
    tag: &'static str,
    found: impl FnOnce(Option<&'static [u8]>) -> Result<B>,
) -> Result<Infallible>
where
    B: FileSystemBackend + Send + 'static,
{
    if let Some(log) = log {
        install_log(log.into_handle().into_raw(), tag);
    }
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
        service: Port::create()?,
        next_badge: AtomicU64::new(0),
    }));
    // Потоки экспорта и монтирования работают до конца процесса.
    let _ = spawn_owned(
        move || export::serve(start, shared),
        EXPORT_STACK_SIZE,
        Priority::new(1),
    )?;
    let _ = spawn_owned(
        move || mount::serve(start, namespace, shared),
        MOUNT_STACK_SIZE,
        Priority::new(1),
    )?;
    log_line("started");
    serve_sessions(shared);
    Err(Error::Ipc(IpcError::PeerClosed))
}

fn serve_sessions(shared: &Shared) {
    let transport = PortTransport::new(shared.service.borrow());
    let mut bytes = [0u8; MESSAGE_INLINE_MAX];
    loop {
        let incoming = match transport.recv(&mut bytes, Timeout::INFINITE) {
            Ok(incoming) => incoming,
            // Уход значка кадром не сопровождается. Сессия ушедшей стороны убирается, а приём
            // продолжается.
            Err(RecvError::SenderGone(badge)) => {
                shared.state.lock().remove_session(badge);
                continue;
            }
            // Отброшенный кадр относится к одному вызову, а бессрочный приём срока не имеет.
            Err(RecvError::Discarded | RecvError::Timeout) => continue,
            Err(RecvError::Closed) => break,
        };
        let mut frame = Frame::default();
        let mut serving = Serving {
            state: shared.state.lock(),
            frame: &mut frame,
            service: &shared.service,
            next_badge: &shared.next_badge,
        };
        let delivered = file_system::dispatch(&mut serving, incoming).is_ok();
        drop(serving);
        frame.end(delivered, &mut shared.state.lock());
    }
    log_line("service port closed");
}
