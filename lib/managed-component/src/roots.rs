use core::num::NonZeroU64;

use component_abi::ComponentStartupClient;
use ipc::wire::Str;
use runtime::{BorrowedHandle, MemoryRegion, OwnedHandle, Port, Timeout};
use syscall::Rights;
use userland::EntryView;

use crate::component::{BoundedStartupTransport, ManagedComponent, ManagedStartupError};

const CONTROL_LEASE_BADGE: NonZeroU64 = NonZeroU64::new(1).expect("lease badge is non-zero");

struct ComponentServers {
    control: OwnedHandle,
    service: OwnedHandle,
}

/// Capability roots компонента, остающиеся у supervisor.
pub struct ComponentRoots {
    control: OwnedHandle,
    service_minter: OwnedHandle,
    servers: Option<ComponentServers>,
}

impl ComponentRoots {
    /// Создаёт control- и service-порты компонента.
    ///
    /// Порты принадлежат одной incarnation: их корни уезжают её процессу, а
    /// оставшиеся у supervisor стороны `READ` не несут.
    pub fn create() -> Result<Self, ManagedStartupError> {
        // Значок lease-капы control-порта: supervisor остаётся его единственным
        // отправителем, и его уход завершает процесс компонента.
        let control = Port::create()?;
        let control_client = control
            .handle()
            .duplicate(Rights::WRITE | Rights::TRANSFER, Some(CONTROL_LEASE_BADGE))?;
        let service = Port::create()?;
        let service_minter = service
            .handle()
            .duplicate(Rights::WRITE | Rights::TRANSFER | Rights::DUPLICATE, None)?;

        Ok(Self {
            control: control_client,
            service_minter,
            servers: Some(ComponentServers {
                control: control.into_handle(),
                service: service.into_handle(),
            }),
        })
    }

    /// Минтит клиентскую capability service-порта под значком `badge`.
    pub fn session(&self, badge: NonZeroU64) -> Result<OwnedHandle, runtime::Error> {
        self.service_minter
            .duplicate(Rights::WRITE | Rights::TRANSFER, Some(badge))
    }

    /// Заимствует постоянную клиентскую сторону control-порта.
    #[must_use]
    pub fn control(&self) -> BorrowedHandle<'_> {
        self.control.borrow()
    }

    /// Заимствует минтер клиентских сессий service-порта.
    #[must_use]
    pub fn service_minter(&self) -> BorrowedHandle<'_> {
        self.service_minter.borrow()
    }

    fn send_startup(
        &mut self,
        transport: BoundedStartupTransport,
        log: &OwnedHandle,
        image: &MemoryRegion,
        config: &str,
    ) -> Result<(), ManagedStartupError> {
        // Значок лог-сессии set-once: производная несёт значок компонента, а
        // `DUPLICATE` нужно ему для делегирования своим потомкам.
        let log = log.duplicate(Rights::WRITE | Rights::TRANSFER | Rights::DUPLICATE, None)?;
        let image = image
            .handle()
            .duplicate(Rights::READ | Rights::TRANSFER | Rights::DUPLICATE, None)?;
        let config = Str::new(config).ok_or(ManagedStartupError::InvalidState)?;
        let servers = self
            .servers
            .take()
            .ok_or(ManagedStartupError::InvalidState)?;
        ComponentStartupClient::new(transport)
            .start(log, image, servers.control, servers.service, config)
            .map_err(ManagedStartupError::from)
    }
}

/// Поднимает incarnation компонента: свежие порты, производные лог-сессии и
/// образа, конечный срок стартового рандеву и путь записи конфигурации.
pub fn launch_component<'a, V: EntryView<'a>>(
    component: &mut ManagedComponent<ComponentRoots>,
    program: &V,
    image: &MemoryRegion,
    log: &OwnedHandle,
    startup_timeout: Timeout,
    config: &str,
) -> Result<(), ManagedStartupError> {
    component.launch(
        program,
        startup_timeout,
        ComponentRoots::create,
        |transport, roots| roots.send_startup(transport, log, image, config),
    )
}
