//! Экспорт `FileSystem` и приём кадров файловых сессий всех томов процесса.

use alloc::boxed::Box;
use core::{convert::Infallible, sync::atomic::AtomicU64};

use component_abi::Attributes;
use filesystem::FileSystemBackend;
use filesystem_abi::{FileSystemError, file_system};
use ipc::{
    Listener, RecvError,
    wire::{IpcError, MESSAGE_INLINE_MAX},
};
use kernel_objects::{MemoryRegion, Port, ReadOnlyMapping, Resource};
use runtime::{Mutex, PortTransport, log_line, process_exit, spawn_owned};
use spin::Once;
use syscall::{Handle, Timeout, kind};

use crate::{
    error::{Error, Result, no_space},
    service::{FileSystemState, Frame, Serving},
    session,
    volume::{DEFAULT_VOLUME, VOLUME_LABEL, Volume, Volumes},
};

static IMAGE: Once<ReadOnlyMapping> = Once::new();

pub(crate) struct Shared {
    pub(crate) volumes: Mutex<Volumes>,
    pub(crate) service: Port,
    pub(crate) next_badge: AtomicU64,
}

/// Строит бекенд из `backing` либо из `None` и обслуживает экспорт и файловые сессии одного
/// тома при любых атрибутах заявки.
pub fn serve<B>(
    start: Handle<kind::Port>,
    backing: Option<MemoryRegion>,
    found: impl FnOnce(Option<&'static [u8]>) -> Result<B>,
) -> Result<Infallible>
where
    B: FileSystemBackend + Send + 'static,
{
    let bytes = match backing.as_ref() {
        Some(backing) => {
            let mapping = backing.map_full_read_only()?;
            Some(IMAGE.call_once(|| mapping).as_bytes())
        }
        None => None,
    };
    let backend = found(bytes)?;
    let state = FileSystemState::new(Box::new(backend), backing, Resource::self_resource()?);

    run(start, state, |shared, _| {
        session::open_session(shared, DEFAULT_VOLUME)
    })
}

/// Обслуживает экспорт и файловые сессии томов, которые строит `volume`. Том называет значение
/// атрибута `label` заявки. Первая заявка с меткой строит дерево, следующие получают то же.
/// Заявка без `label` получает том по умолчанию, построенный до первой заявки. Тома делят
/// бюджет процесса.
pub fn serve_volumes<B>(
    start: Handle<kind::Port>,
    mut volume: impl FnMut() -> Result<B> + Send + 'static,
) -> Result<Infallible>
where
    B: FileSystemBackend + Send + 'static,
{
    let state = FileSystemState::new(Box::new(volume()?), None, Resource::self_resource()?);

    run(start, state, move |shared, attributes| {
        let label = attributes.value(VOLUME_LABEL).unwrap_or_default();
        let found = shared.volumes.lock().find(label);
        let index = if let Some(index) = found {
            index
        } else {
            let backend = volume().map_err(|_| FileSystemError::NoSpace)?;
            let state = FileSystemState::new(
                Box::new(backend),
                None,
                Resource::self_resource().map_err(no_space)?,
            );
            shared.volumes.lock().insert(Volume::new(label, state)?)?
        };
        session::open_session(shared, index)
    })
}

fn run(
    start: Handle<kind::Port>,
    first: FileSystemState,
    mut open: impl FnMut(&'static Shared, Attributes<'_>) -> core::result::Result<Port, FileSystemError>
    + Send
    + 'static,
) -> Result<Infallible> {
    let shared: &'static Shared = Box::leak(Box::new(Shared {
        volumes: Mutex::new(Volumes::new(first)),
        service: Port::create()?,
        next_badge: AtomicU64::new(0),
    }));
    let _ = spawn_owned(move || {
        let parent = component_abi::component_startup::Client::new(PortTransport::new(start));
        let _ = component_startup::serve_export::<file_system::Protocol>(&parent, |attributes| {
            open(shared, attributes).map_err(|_| component_startup::SessionError::NoSpace)
        });
        process_exit(1)
    })?;
    serve_sessions(shared);
    Err(Error::Ipc(IpcError::PeerClosed))
}

fn serve_sessions(shared: &Shared) {
    let transport = PortTransport::new(shared.service.borrow());
    let mut bytes = [0u8; MESSAGE_INLINE_MAX];
    loop {
        let incoming = match transport.recv(&mut bytes, Timeout::INFINITE) {
            Ok(incoming) => incoming,
            // Сессия ушедшей стороны убирается, а приём продолжается.
            Err(RecvError::SenderGone(badge)) => {
                shared.volumes.lock().remove_session(badge);
                continue;
            }
            // Отброшенный кадр относится к одному вызову, а бессрочный приём срока не имеет.
            Err(RecvError::Discarded | RecvError::Timeout) => continue,
            Err(RecvError::Closed) => break,
        };
        // Том выбирает значок сессии до разбора кадра. Кадр без известной сессии получает
        // отказ `NoSession` от тома по умолчанию.
        let mut volumes = shared.volumes.lock();
        let volume = volumes.of(incoming.resources.badge);
        let mut frame = Frame::default();
        let mut serving = Serving {
            state: volumes.state(volume),
            frame: &mut frame,
            service: &shared.service,
            next_badge: &shared.next_badge,
        };
        let delivered = file_system::dispatch(&mut serving, incoming).is_ok();
        frame.end(delivered, volumes.state(volume));
    }
    log_line("service port closed");
}
