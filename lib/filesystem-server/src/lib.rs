//! Обслуживание экспорта `FileSystem` и сессий файловой системы.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use core::{convert::Infallible, sync::atomic::AtomicU64};

use filesystem::FileSystemBackend;
use filesystem_abi::file_system;
use ipc::{
    Listener, RecvError,
    wire::{IpcError, MESSAGE_INLINE_MAX},
};
use kernel_objects::{MemoryRegion, Port, ReadOnlyMapping, Resource};
use runtime::{Mutex, PortTransport, Priority, log_line, process_exit, spawn_owned};
use spin::Once;
use syscall::{Handle, Timeout, kind};

mod buffer;
mod error;
mod service;
mod session;
mod store;

pub use error::{Error, Result};
use service::{FileSystemState, Frame, Serving};

static IMAGE: Once<ReadOnlyMapping> = Once::new();

struct Shared {
    state: Mutex<FileSystemState>,
    service: Port,
    next_badge: AtomicU64,
}

/// Строит бекенд из `backing` либо из `None` и обслуживает экспорт и файловые сессии.
pub fn serve<B>(
    start: Handle<kind::Port>,
    backing: Option<MemoryRegion>,
    found: impl FnOnce(Option<&'static [u8]>) -> Result<B>,
) -> Result<Infallible>
where
    B: FileSystemBackend + Send + 'static,
{
    const EXPORT_THREAD_STACK_SIZE: u64 = 0x8000;

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
    let _ = spawn_owned(
        move || {
            let parent = component_abi::component_startup::Client::new(PortTransport::new(start));
            let _ = component_startup::serve_export::<file_system::Protocol>(&parent, || {
                session::open_session(shared).map_err(|_| component_startup::SessionError::NoSpace)
            });
            process_exit(1)
        },
        EXPORT_THREAD_STACK_SIZE,
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
