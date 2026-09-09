use core::num::NonZeroU64;

use component_abi::{COMPONENT_EXPORT_MAX, COMPONENT_SLOT_MAX, component_startup};
use kernel_objects::{BorrowedHandle, ObjectKind, OwnedHandle, Port, kind};
use runtime::Timeout;
use syscall::Rights;
use userland::EntryView;

use crate::component::{BoundedStartupTransport, ManagedComponent, ManagedStartupError};

/// Capability слотов компонента в порядке плана. Недоставленные слоты
/// закрываются вместе с ней.
#[derive(Default)]
pub struct ComponentSlots {
    slots: [Option<OwnedHandle<kind::Any>>; COMPONENT_SLOT_MAX],
    count: u16,
}

/// Capability roots компонента, остающиеся у supervisor: по минтеру на каждый экспорт и
/// серверные стороны их портов до отправки стартовой транзакции.
pub struct ComponentRoots {
    minters: [Option<Port>; COMPONENT_EXPORT_MAX],
    export_count: usize,
    servers: Option<[Option<Port>; COMPONENT_EXPORT_MAX]>,
}

impl ComponentSlots {
    /// Добавляет capability очередным слотом. Слоты одного плана разных видов, и вид
    /// снимается на входе в общий массив.
    pub fn push<K: ObjectKind>(
        &mut self,
        capability: OwnedHandle<K>,
    ) -> Result<(), ManagedStartupError> {
        let place = self
            .slots
            .get_mut(usize::from(self.count))
            .ok_or(ManagedStartupError::TooManySlots)?;
        *place = Some(capability.erase());
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
    /// Создаёт по порту на каждый из `export_count` экспортов компонента.
    ///
    /// Порты принадлежат одной incarnation: их корни уезжают её процессу, а
    /// оставшиеся у supervisor стороны `READ` не дают. Минтеры выводятся из
    /// корня, пока тот у supervisor: заклеймённая капа значка уже не примет.
    pub fn create(export_count: usize) -> Result<Self, ManagedStartupError> {
        if export_count > COMPONENT_EXPORT_MAX {
            return Err(ManagedStartupError::TooManyExports);
        }
        let minted = Rights::WRITE | Rights::TRANSFER | Rights::DUPLICATE;
        let mut minters: [Option<Port>; COMPONENT_EXPORT_MAX] = core::array::from_fn(|_| None);
        let mut servers: [Option<Port>; COMPONENT_EXPORT_MAX] = core::array::from_fn(|_| None);
        for (minter, server) in minters.iter_mut().zip(&mut servers).take(export_count) {
            let port = Port::create()?;
            *minter = Some(port.duplicate(minted, None)?);
            *server = Some(port);
        }

        Ok(Self {
            minters,
            export_count,
            servers: Some(servers),
        })
    }

    /// Число экспортов компонента.
    #[must_use]
    pub const fn export_count(&self) -> usize {
        self.export_count
    }

    /// Минтит клиентскую capability порта экспорта `export` под значком `badge`.
    pub fn session(&self, export: usize, badge: NonZeroU64) -> Result<Port, kernel_objects::Error> {
        self.minter(export)?
            .duplicate(Rights::WRITE | Rights::TRANSFER, Some(badge))
    }

    /// Заимствует минтер клиентских сессий порта экспорта `export`.
    pub fn export_minter(
        &self,
        export: usize,
    ) -> Result<BorrowedHandle<'_, kind::Port>, kernel_objects::Error> {
        Ok(self.minter(export)?.borrow())
    }

    fn minter(&self, export: usize) -> Result<&Port, kernel_objects::Error> {
        self.minters
            .get(export)
            .filter(|_| export < self.export_count)
            .and_then(Option::as_ref)
            .ok_or(kernel_objects::Error::Syscall(
                syscall::SyscallError::BadHandle,
            ))
    }

    fn send_startup(
        &mut self,
        transport: BoundedStartupTransport,
        log: &Port,
        slots: ComponentSlots,
    ) -> Result<(), ManagedStartupError> {
        // Значок лог-сессии set-once: производная содержит значок компонента, а
        // `DUPLICATE` нужно ему для делегирования своим потомкам.
        let log = log.duplicate(Rights::WRITE | Rights::TRANSFER | Rights::DUPLICATE, None)?;
        let servers = self
            .servers
            .take()
            .ok_or(ManagedStartupError::InvalidState)?;
        let client = component_startup::Client::new(transport);
        client.begin(log, export_count(self.export_count)?, slots.len())?;
        for server in servers.into_iter().flatten() {
            client.export(server)?;
        }
        for slot in slots.slots.into_iter().flatten() {
            client.slot(slot)?;
        }
        Ok(())
    }
}

fn export_count(count: usize) -> Result<u16, ManagedStartupError> {
    u16::try_from(count).map_err(|_| ManagedStartupError::TooManyExports)
}

/// Поднимает incarnation компонента: порт на каждый из `export_count` экспортов,
/// производная лог-сессия, конечный срок стартового рандеву и слоты плана. `name`
/// называет процесс экземпляра.
pub fn launch_component<'a, V: EntryView<'a>>(
    component: &mut ManagedComponent<ComponentRoots>,
    program: &V,
    name: &str,
    log: &Port,
    startup_timeout: Timeout,
    export_count: usize,
    slots: ComponentSlots,
) -> Result<(), ManagedStartupError> {
    component.launch(
        program,
        name,
        startup_timeout,
        move || ComponentRoots::create(export_count),
        move |transport, roots| roots.send_startup(transport, log, slots),
    )
}
