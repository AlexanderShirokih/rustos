use core::num::NonZeroU64;

use filesystem_abi::{FileSystemError, NamespaceAdminClient, NamespaceStartupClient, PATH_MAX};
use ipc::wire::{IpcError, Str};
use runtime::{BorrowedHandle, MemoryRegion, OwnedHandle, Port, PortTransport, Resource, Timeout};
use syscall::Rights;
use userland::EntryView;

use crate::{BoundedStartupTransport, ManagedComponent, ManagedStartupError};

/// Монтирование файловой системы под префиксом дерева имён.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mount<'a> {
    /// Префикс дерева имён.
    pub path: &'a str,
    /// Запись userland-образа с программой файловой системы.
    pub program: &'a str,
}

/// Параметры одного запуска filesystem namespace.
#[derive(Debug, Clone, Copy)]
pub struct NamespaceBootConfig<'a> {
    /// Отдельный бюджет router-а в страницах.
    pub budget_pages: u64,
    /// Конечный срок доставки startup-кадра.
    pub startup_timeout: Timeout,
    /// Конечный срок каждого admin-вызова.
    pub admin_timeout: Timeout,
    /// Конечный срок остановки router-а при rollback.
    pub stop_timeout: Timeout,
    /// Дерево имён, поднимаемое в порядке перечисления.
    pub mounts: &'a [Mount<'a>],
}

/// Ошибка boot-транзакции filesystem namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamespaceBootError {
    /// Managed startup router-а не завершился.
    Startup(ManagedStartupError),
    /// Один из mount-вызовов отказал, rollback завершён.
    Mount(NamespaceMountError),
    /// При отказе mount не удалось полностью остановить router.
    Cleanup {
        // Исходный отказ mount-транзакции.
        mount: NamespaceMountError,
        // Отказ terminate/join managed-компонента.
        cleanup: ManagedStartupError,
    },
}

/// Отказ поднятия дерева имён на одном из его монтирований.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NamespaceMountError {
    /// Позиция отказавшего монтирования в таблице.
    pub index: usize,
    /// Причина отказа монтирования.
    pub failure: MountFailure,
    /// Причина отказа снятия уже поднятых монтирований.
    pub rollback: Option<MountFailure>,
}

/// Причина отказа одного admin-вызова маршрутизатора.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountFailure {
    /// Транспорт admin-вызова отказал.
    Transport(IpcError),
    /// Маршрутизатор отклонил вызов.
    Rejected(FileSystemError),
}

// Значок lease-капы admin-порта маршрутизатора: supervisor остаётся его единственным
// отправителем, и его уход завершает процесс маршрутизатора.
const ROUTER_LEASE_BADGE: NonZeroU64 = NonZeroU64::new(1).expect("lease badge is non-zero");

struct NamespaceServers {
    admin: OwnedHandle,
    service: OwnedHandle,
}

/// Capability roots маршрутизатора имён, остающиеся у rootkeeper.
pub struct NamespaceRoots {
    admin: OwnedHandle,
    service_minter: OwnedHandle,
    servers: Option<NamespaceServers>,
}

impl NamespaceRoots {
    /// Создаёт раздельные admin- и service-порты маршрутизатора.
    ///
    /// Порты принадлежат одной incarnation: их корни уезжают её процессу, а
    /// оставшиеся у rootkeeper стороны `READ` не несут.
    pub fn create() -> Result<Self, ManagedStartupError> {
        let admin = Port::create()?;
        let admin_client = admin
            .handle()
            .duplicate(Rights::WRITE | Rights::TRANSFER, Some(ROUTER_LEASE_BADGE))?;
        let service = Port::create()?;
        let service_minter = service
            .handle()
            .duplicate(Rights::WRITE | Rights::TRANSFER | Rights::DUPLICATE, None)?;

        Ok(Self {
            admin: admin_client,
            service_minter,
            servers: Some(NamespaceServers {
                admin: admin.into_handle(),
                service: service.into_handle(),
            }),
        })
    }

    /// Передаёт router-у startup capability и серверные корни его портов.
    ///
    /// Лог-сессия и образ минтятся производными: их источники переживают
    /// incarnation.
    pub fn send_startup(
        &mut self,
        transport: BoundedStartupTransport,
        log: &OwnedHandle,
        image: &MemoryRegion,
    ) -> Result<(), ManagedStartupError> {
        // Значок лог-сессии set-once: производная несёт значок подсистемы, а
        // `DUPLICATE` нужно маршрутизатору для делегирования монтированиям.
        let log = log.duplicate(Rights::WRITE | Rights::TRANSFER | Rights::DUPLICATE, None)?;
        let image = image
            .handle()
            .duplicate(Rights::READ | Rights::TRANSFER | Rights::DUPLICATE, None)?;
        let servers = self
            .servers
            .take()
            .ok_or(ManagedStartupError::InvalidState)?;
        NamespaceStartupClient::new(transport)
            .start(log, image, servers.service, servers.admin)
            .map_err(ManagedStartupError::from)
    }

    /// Монтирует программу файловой системы под `path` через admin-порт.
    pub fn mount(&self, mount: Mount<'_>, timeout: Timeout) -> Result<(), MountFailure> {
        let path = wire_path(mount.path)?;
        let program = wire_path(mount.program)?;
        NamespaceAdminClient::new(PortTransport::client(&self.admin))
            .with_wait(timeout)
            .mount(path, program)
            .map_err(MountFailure::Transport)?
            .map(|_| ())
            .map_err(MountFailure::Rejected)
    }

    /// Снимает монтирование под `path` через admin-порт.
    pub fn unmount(&self, path: &str, timeout: Timeout) -> Result<(), MountFailure> {
        let path = wire_path(path)?;
        NamespaceAdminClient::new(PortTransport::client(&self.admin))
            .with_wait(timeout)
            .unmount(path)
            .map_err(MountFailure::Transport)?
            .map(|_| ())
            .map_err(MountFailure::Rejected)
    }

    /// Минтит клиентскую capability опубликованного Namespace-сервиса.
    pub fn session(&self, badge: NonZeroU64) -> Result<OwnedHandle, runtime::Error> {
        self.service_minter
            .duplicate(Rights::WRITE | Rights::TRANSFER, Some(badge))
    }

    /// Заимствует постоянную клиентскую сторону admin-порта.
    #[must_use]
    pub fn admin(&self) -> BorrowedHandle<'_> {
        self.admin.borrow()
    }

    /// Заимствует минтер клиентских Namespace-сессий.
    #[must_use]
    pub fn service_minter(&self) -> BorrowedHandle<'_> {
        self.service_minter.borrow()
    }
}

/// Заводит компонент маршрутизатора на бюджете `config.budget_pages` и
/// поднимает его первую incarnation вместе с деревом имён.
pub fn launch_namespace<'a, V: EntryView<'a>>(
    program: &V,
    parent_resource: &Resource,
    image: &MemoryRegion,
    log: &OwnedHandle,
    config: NamespaceBootConfig<'_>,
) -> Result<ManagedComponent<NamespaceRoots>, NamespaceBootError> {
    let mut component = ManagedComponent::new(parent_resource, config.budget_pages)
        .map_err(NamespaceBootError::Startup)?;
    relaunch_namespace(&mut component, program, image, log, config)?;
    Ok(component)
}

/// Поднимает новую incarnation маршрутизатора и транзакционно монтирует дерево
/// имён `config.mounts`.
///
/// Incarnation получает свежие порты, свежую производную лог-сессии и ресурс из
/// резерва компонента. Отказ монтирования снимает уже поднятые префиксы и
/// завершает маршрутизатор.
pub fn relaunch_namespace<'a, V: EntryView<'a>>(
    component: &mut ManagedComponent<NamespaceRoots>,
    program: &V,
    image: &MemoryRegion,
    log: &OwnedHandle,
    config: NamespaceBootConfig<'_>,
) -> Result<(), NamespaceBootError> {
    component
        .launch(
            program,
            config.startup_timeout,
            NamespaceRoots::create,
            |transport, roots| roots.send_startup(transport, log, image),
        )
        .map_err(NamespaceBootError::Startup)?;

    let mounted = match component.running() {
        Some(running) => mount_all(running.roots(), config.mounts, config.admin_timeout),
        None => {
            return Err(NamespaceBootError::Startup(
                ManagedStartupError::InvalidState,
            ));
        }
    };
    if let Err(mount) = mounted {
        return match stop_namespace(component, ROLLBACK_EXIT_CODE, config.stop_timeout) {
            Ok(()) => Err(NamespaceBootError::Mount(mount)),
            Err(cleanup) => Err(NamespaceBootError::Cleanup { mount, cleanup }),
        };
    }
    Ok(())
}

/// Завершает живой managed router и сохраняет его exit code.
pub fn stop_namespace(
    component: &mut ManagedComponent<NamespaceRoots>,
    exit_code: u32,
    timeout: Timeout,
) -> Result<(), ManagedStartupError> {
    let running = component
        .running()
        .ok_or(ManagedStartupError::InvalidState)?;
    running
        .process()
        .terminate(exit_code)
        .map_err(ManagedStartupError::Runtime)?;
    component.observe_exit(timeout)
}

// Снимает дерево имён в порядке, обратном его поднятию.
pub(crate) fn unmount_all(roots: &NamespaceRoots, mounts: &[Mount<'_>], timeout: Timeout) {
    for mount in mounts.iter().rev() {
        let _ = roots.unmount(mount.path, timeout);
    }
}

// Код выхода router-а, снимаемого откатом неполного дерева имён.
const ROLLBACK_EXIT_CODE: u32 = 1;

// Отказ монтирования снимает уже поднятые префиксы: наружу дерево имён уходит
// либо целиком поднятым, либо не поднятым вовсе.
fn mount_all(
    roots: &NamespaceRoots,
    mounts: &[Mount<'_>],
    timeout: Timeout,
) -> Result<(), NamespaceMountError> {
    for (index, mount) in mounts.iter().enumerate() {
        let Err(failure) = roots.mount(*mount, timeout) else {
            continue;
        };
        let rollback = mounts[..index].iter().rev().fold(None, |first, done| {
            first.or(roots.unmount(done.path, timeout).err())
        });
        return Err(NamespaceMountError {
            index,
            failure,
            rollback,
        });
    }
    Ok(())
}

fn wire_path(path: &str) -> Result<Str<'_, PATH_MAX>, MountFailure> {
    Str::new(path).ok_or(MountFailure::Transport(IpcError::InvalidValue))
}
