//! Экспорт `FileSystem` и приём кадров файловых сессий всех томов процесса.

use alloc::{
    boxed::Box,
    rc::{Rc, Weak},
};
use core::convert::Infallible;

use component_abi::Attributes;
use component_startup::{AsyncStartup, SessionError, SessionPort, serve_component};
use executor::{Executor, PortChannel};
use filesystem::FileSystemBackend;
use filesystem_abi::{FileSystemError, file_system};
use ipc::wire::IpcError;
use kernel_objects::{MemoryRegion, Port, ReadOnlyMapping, Resource};
use runtime::{Mutex, log_fmt};
use spin::Once;
use syscall::{Handle, kind};

use crate::{
    control::Control,
    error::{Error, Result, no_space},
    service::FileSystemState,
    session,
    volume::{Volume, Volumes},
};

static IMAGE: Once<ReadOnlyMapping> = Once::new();

pub(crate) struct Shared {
    pub(crate) volumes: Mutex<Volumes>,
    pub(crate) service: Rc<SessionPort>,
    pub(crate) executor: Weak<Executor>,
}

impl Shared {
    pub(crate) fn with_state<T>(
        &self,
        volume: usize,
        operation: impl FnOnce(&mut FileSystemState) -> core::result::Result<T, FileSystemError>,
    ) -> core::result::Result<T, FileSystemError> {
        let mut volumes = self.volumes.lock();
        operation(volumes.state(volume).ok_or(FileSystemError::NoSession)?)
    }
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
    mut open: impl FnMut(&Shared, Attributes<'_>) -> core::result::Result<Port, SessionError> + 'static,
) -> Result<Infallible> {
    let executor = Rc::new(Executor::new()?);
    let shared = Rc::new(Shared {
        volumes: Mutex::new(volumes),
        service: Rc::new(SessionPort::create()?),
        executor: Rc::downgrade(&executor),
    });
    let calls = shared.clone();
    let export = shared.clone();
    let gone = shared.clone();
    let served = serve_component::<file_system::Protocol, _>(
        &executor,
        AsyncStartup::new(PortChannel::new(start, executor.reactor())),
        shared.service.clone(),
        move |incoming| {
            let volume = calls.volumes.lock().of(incoming.resources.badge);
            let control = Control {
                shared: calls.clone(),
                volume,
            };
            file_system::serve(&control, incoming)
        },
        async move |attributes| open(&export, attributes),
        move |badge| {
            let exchange = gone.volumes.lock().remove_session(badge);
            drop(exchange);
        },
    );
    log_fmt(format_args!(
        "filesystem stopped: {}",
        executor.block_on(served)
    ));
    Err(Error::Ipc(IpcError::PeerClosed))
}
