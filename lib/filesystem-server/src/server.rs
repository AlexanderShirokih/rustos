//! Экспорт `FileSystem` и приём кадров файловых сессий всех томов процесса.

use alloc::boxed::Box;
use core::{convert::Infallible, sync::atomic::AtomicU64};

use component_abi::Attributes;
use component_startup::SessionError;
use filesystem::FileSystemBackend;
use filesystem_abi::file_system;
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
    volume::{Volume, Volumes},
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
    let image = match backing {
        Some(backing) => {
            let mapping = ReadOnlyMapping::new(backing).map_err(kernel_objects::Error::from)?;
            Some(IMAGE.call_once(|| mapping))
        }
        None => None,
    };
    let backend = found(image.map(ReadOnlyMapping::as_bytes))?;
    let state = FileSystemState::new(Box::new(backend), image, Resource::self_resource()?);
    let mut volumes = Volumes::default();
    let volume = volumes.insert(Volume::new(Attributes::empty(), state))?;

    run(start, volumes, move |shared, _| {
        session::open_session(shared, volume).map_err(|_| SessionError::NoSpace)
    })
}

/// Обслуживает экспорт и файловые сессии томов, которые строит `volume`. Заявки с одинаковым
/// набором атрибутов получают сессии одного тома, порядок пар в заявке значения не имеет. Первая
/// заявка с новым набором вызывает `volume` со своими атрибутами. Если `volume` отказал, заявка
/// получает этот отказ, и том не создаётся. Тома делят бюджет процесса.
pub fn serve_volumes<B>(
    start: Handle<kind::Port>,
    mut volume: impl FnMut(Attributes<'_>) -> core::result::Result<B, SessionError> + Send + 'static,
) -> Result<Infallible>
where
    B: FileSystemBackend + Send + 'static,
{
    run(start, Volumes::default(), move |shared, attributes| {
        let found = shared.volumes.lock().find(attributes);
        let index = if let Some(index) = found {
            index
        } else {
            let backend = volume(attributes)?;
            Resource::self_resource()
                .map_err(no_space)
                .map(|resource| FileSystemState::new(Box::new(backend), None, resource))
                .and_then(|state| shared.volumes.lock().insert(Volume::new(attributes, state)))
                .map_err(|_| SessionError::NoSpace)?
        };
        session::open_session(shared, index).map_err(|_| SessionError::NoSpace)
    })
}

fn run(
    start: Handle<kind::Port>,
    volumes: Volumes,
    mut open: impl FnMut(&'static Shared, Attributes<'_>) -> core::result::Result<Port, SessionError>
    + Send
    + 'static,
) -> Result<Infallible> {
    let shared: &'static Shared = Box::leak(Box::new(Shared {
        volumes: Mutex::new(volumes),
        service: Port::create()?,
        next_badge: AtomicU64::new(0),
    }));
    let _ = spawn_owned(move || {
        let parent = component_abi::component_startup::Client::new(PortTransport::new(start));
        let _ = component_startup::serve_export::<file_system::Protocol>(&parent, |attributes| {
            open(shared, attributes)
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
        // отказ `NoSession` от первого тома. Таблица при кадре не пуста, потому что значок
        // выдаётся только сессии тома, а тома не удаляются.
        let mut volumes = shared.volumes.lock();
        let volume = volumes.of(incoming.resources.badge);
        let Some(state) = volumes.state(volume) else {
            continue;
        };
        let mut frame = Frame::default();
        let mut serving = Serving {
            state: &mut *state,
            frame: &mut frame,
            service: &shared.service,
            next_badge: &shared.next_badge,
        };
        let delivered = file_system::dispatch(&mut serving, incoming).is_ok();
        frame.end(delivered, state);
    }
    log_line("service port closed");
}
