use core::num::NonZeroU64;

use component_abi::{COMPONENT_SLOT_MAX, ComponentStartupClient};
use runtime::{BorrowedHandle, OwnedHandle, Port, Timeout};
use syscall::Rights;
use userland::EntryView;

use crate::component::{BoundedStartupTransport, ManagedComponent, ManagedStartupError};

const CONTROL_LEASE_BADGE: NonZeroU64 = NonZeroU64::new(1).expect("lease badge is non-zero");

struct ComponentServers {
    control: OwnedHandle,
    service: OwnedHandle,
}

/// Capability слотов компонента в порядке плана. Недоставленные слоты
/// закрываются вместе с ней.
#[derive(Default)]
pub struct ComponentSlots {
    slots: [Option<OwnedHandle>; COMPONENT_SLOT_MAX],
    count: u16,
}

/// Capability roots компонента, остающиеся у supervisor.
pub struct ComponentRoots {
    control: OwnedHandle,
    service_minter: OwnedHandle,
    servers: Option<ComponentServers>,
}

impl ComponentSlots {
    /// Добавляет capability очередным слотом.
    pub fn push(&mut self, capability: OwnedHandle) -> Result<(), ManagedStartupError> {
        let place = self
            .slots
            .get_mut(usize::from(self.count))
            .ok_or(ManagedStartupError::TooManySlots)?;
        *place = Some(capability);
        self.count += 1;
        Ok(())
    }

    /// Число заполненных слотов.
    #[must_use]
    pub const fn len(&self) -> u16 {
        self.count
    }

    /// Компонент без слотов capability не получает.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }
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
        slots: ComponentSlots,
    ) -> Result<(), ManagedStartupError> {
        // Значок лог-сессии set-once: производная несёт значок компонента, а
        // `DUPLICATE` нужно ему для делегирования своим потомкам.
        let log = log.duplicate(Rights::WRITE | Rights::TRANSFER | Rights::DUPLICATE, None)?;
        let servers = self
            .servers
            .take()
            .ok_or(ManagedStartupError::InvalidState)?;
        let client = ComponentStartupClient::new(transport);
        client.start(log, servers.control, servers.service, slots.len())?;
        for slot in slots.slots.into_iter().flatten() {
            client.slot(slot)?;
        }
        Ok(())
    }
}

/// Поднимает incarnation компонента: свежие порты, производная лог-сессия,
/// конечный срок стартового рандеву и слоты плана.
pub fn launch_component<'a, V: EntryView<'a>>(
    component: &mut ManagedComponent<ComponentRoots>,
    program: &V,
    log: &OwnedHandle,
    startup_timeout: Timeout,
    slots: ComponentSlots,
) -> Result<(), ManagedStartupError> {
    component.launch(
        program,
        startup_timeout,
        ComponentRoots::create,
        move |transport, roots| roots.send_startup(transport, log, slots),
    )
}
