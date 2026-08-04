//! Композиция, которую rootkeeper поднимает при загрузке, и её supervisor.

use core::fmt::Write as _;

use bootstrap::BootstrapClient;
use log_abi::{LogBuffer, LogClient};
use runtime::{
    MemoryRegion, OwnedHandle, PortTransport, ReadOnlyMapping, Resource, Timeout, wait_any,
};
use syscall::{Handle, SIGNALED, WaitItem};
use userland_image::decode;

use crate::{
    ManagedComponent, Mount, NamespaceBootConfig, NamespaceBootError, NamespaceRoots,
    launch_namespace, namespace::unmount_all, relaunch_namespace, relay, stop_namespace,
};

// Тег строк supervisor в журнале.
const LOG_TAG: &str = "supervisor";

/// Конечный срок остановки подсистемы: terminate маршрутизатора тянет за собой
/// join каждого его монтирования, и каждое отвечает в пределах admin-срока.
pub const STOP_TIMEOUT: Timeout = Timeout::from_secs(5);

/// Дерево имён загрузки: корень - read-only образ, изменяемое поддерево - в
/// памяти процесса ramfs.
pub const BOOT_MOUNTS: &[Mount<'static>] = &[
    Mount {
        path: "/",
        program: "/boot/filesystem-imagefs",
    },
    Mount {
        path: "/tmp",
        program: "/boot/filesystem-ramfs",
    },
];

/// Исход прохода supervisor: этим кодом ядро гасит машину.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stopped {
    /// Инфраструктура supervisor не поднялась.
    Startup = 1,
    /// Userland-образ недоступен либо не разобран.
    BadImage = 2,
    /// Критическая подсистема завершилась.
    Subsystem = 3,
}

/// Причина, по которой композиция не поднялась.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootFailure {
    /// Лог-сессия подсистемы не выдана.
    LogSession,
    /// Userland-образ недоступен либо не разобран.
    Image,
    /// В образе нет программы маршрутизатора имён.
    RouterMissing,
    /// Корневой metering resource недоступен.
    Resource,
    /// Managed-запуск маршрутизатора не завершился.
    Namespace(NamespaceBootError),
}

/// Исход одного оборота supervisor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Supervised {
    /// Маршрутизатор ушёл с кодом `exit_code`, и композиция подняла его новую
    /// incarnation вместе с деревом имён.
    Restarted { exit_code: Option<u32> },
    /// Композиция остановлена: восстановить её оборот supervisor не смог.
    Stopped(Stopped),
}

/// Поднятая композиция rootkeeper.
pub struct RootSystem {
    namespace: ManagedComponent<NamespaceRoots>,
    mounts: &'static [Mount<'static>],
    // Родительская капа резерва компонента: её закрытие отозвало бы весь бюджет
    // маршрутизатора.
    _resource: Resource,
    // Регион и отображение образа переживают маршрутизатор: каждая incarnation
    // получает производную капу региона, а его байты читает загрузчик.
    image: MemoryRegion,
    mapping: ReadOnlyMapping,
    // Делегированная лог-сессия подсистемы: производную от неё получает каждая
    // incarnation.
    log: OwnedHandle,
}

// Запись образа с программой маршрутизатора имён.
const ROUTER_PROGRAM: &str = "/boot/namespace-router";

// Бюджет маршрутизатора: сверх собственной арены он нарезает из него внутренние
// бюджеты своим монтированиям, по 512 страниц на каждое.
const ROUTER_BUDGET_PAGES: u64 = 2_048;

// Конечный срок startup-рандеву и admin-вызова. Обе операции синхронны и
// укладываются в миллисекунды, а конечный срок превращает молчание
// маршрутизатора в диагностируемый отказ вместо зависшей загрузки.
const CALL_TIMEOUT: Timeout = Timeout::from_secs(2);

// Код выхода маршрутизатора, снимаемого штатным завершением системы.
const SHUTDOWN_EXIT_CODE: u32 = 1;

// Предел запусков маршрутизатора: исчерпав его, композиция останавливается, а
// не поднимает следующую incarnation.
const ROUTER_GENERATION_MAX: u32 = 3;

// Ёмкость строки о завершившейся подсистеме.
const EXIT_MESSAGE_MAX: usize = 96;

struct SystemImage {
    region: MemoryRegion,
    mapping: ReadOnlyMapping,
}

impl From<Stopped> for u64 {
    fn from(stopped: Stopped) -> Self {
        stopped as Self
    }
}

impl BootFailure {
    /// Код выхода supervisor, соответствующий отказу.
    #[must_use]
    pub const fn stopped(self) -> Stopped {
        match self {
            Self::Image | Self::RouterMissing => Stopped::BadImage,
            Self::LogSession | Self::Resource | Self::Namespace(NamespaceBootError::Startup(_)) => {
                Stopped::Startup
            }
            Self::Namespace(_) => Stopped::Subsystem,
        }
    }

    /// Строка отказа для журнала.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::LogSession => "log session not minted",
            Self::Image => "userland image unavailable",
            Self::RouterMissing => "namespace router missing",
            Self::Resource => "root resource unavailable",
            Self::Namespace(_) => "filesystem namespace not started",
        }
    }
}

impl RootSystem {
    /// Поднимает маршрутизатор имён с лог-сессией `log` и деревом имён `mounts`.
    pub fn bring_up(
        kernel: Handle,
        log: OwnedHandle,
        mounts: &'static [Mount<'static>],
    ) -> Result<Self, BootFailure> {
        let SystemImage { region, mapping } = acquire_image(kernel)?;
        let decoded = decode(mapping.as_bytes()).map_err(|_| BootFailure::Image)?;
        let program = decoded
            .entry(ROUTER_PROGRAM)
            .and_then(|entry| entry.program().ok())
            .ok_or(BootFailure::RouterMissing)?;
        let resource = Resource::self_resource().map_err(|_| BootFailure::Resource)?;

        let namespace = launch_namespace(&program, &resource, &region, &log, boot_config(mounts))
            .map_err(BootFailure::Namespace)?;
        Ok(Self {
            namespace,
            mounts,
            _resource: resource,
            image: region,
            mapping,
            log,
        })
    }

    /// Managed-компонент маршрутизатора имён.
    #[must_use]
    pub const fn namespace(&self) -> &ManagedComponent<NamespaceRoots> {
        &self.namespace
    }

    /// Оборот supervisor: ждёт завершения маршрутизатора либо потока
    /// `relay_thread`, записывает исход в журнал ядра и поднимает
    /// маршрутизатору смену.
    ///
    /// Завершение потока пересылки критично: восстановить его композиция не
    /// умеет.
    pub fn supervise_turn(&mut self, kernel: Handle, relay_thread: Handle) -> Supervised {
        let kernel_log = BootstrapClient::new(PortTransport::client(kernel));
        let Some(router) = self
            .namespace
            .running()
            .map(|running| running.process().handle().as_raw())
        else {
            return Supervised::Stopped(Stopped::Subsystem);
        };
        let waits = [
            WaitItem::new(router, SIGNALED),
            WaitItem::new(relay_thread, SIGNALED),
        ];
        match wait_any(&waits, Timeout::INFINITE) {
            Ok(0) => self.replace_router(&kernel_log),
            Ok(_) => {
                log_exit(&kernel_log, "log relay", relay::exit_code());
                Supervised::Stopped(Stopped::Subsystem)
            }
            Err(_) => {
                let _ = kernel_log.log_str(LOG_TAG, "supervisor wait failed");
                Supervised::Stopped(Stopped::Subsystem)
            }
        }
    }

    // Снимает ушедшую incarnation маршрутизатора и поднимает следующую, пока
    // компонент не исчерпал свой предел запусков.
    fn replace_router(&mut self, kernel_log: &BootstrapClient<PortTransport>) -> Supervised {
        let exit_code = self
            .namespace
            .running()
            .and_then(|running| running.process().exit_code().ok());
        let observed = self.namespace.observe_exit(Timeout::POLL);
        log_exit(kernel_log, "namespace router", exit_code);
        if observed.is_err() {
            let _ = kernel_log.log_str(LOG_TAG, "namespace router budget not returned");
            return Supervised::Stopped(Stopped::Subsystem);
        }
        if self.namespace.generation() >= ROUTER_GENERATION_MAX {
            let _ = kernel_log.log_str(LOG_TAG, "namespace router restart limit reached");
            return Supervised::Stopped(Stopped::Subsystem);
        }
        if self.restart().is_err() {
            let _ = kernel_log.log_str(LOG_TAG, "namespace router not restarted");
            return Supervised::Stopped(Stopped::Subsystem);
        }
        Supervised::Restarted { exit_code }
    }

    // Новая incarnation поднимается из того же образа и с тем же деревом имён.
    fn restart(&mut self) -> Result<(), BootFailure> {
        let decoded = decode(self.mapping.as_bytes()).map_err(|_| BootFailure::Image)?;
        let program = decoded
            .entry(ROUTER_PROGRAM)
            .and_then(|entry| entry.program().ok())
            .ok_or(BootFailure::RouterMissing)?;
        relaunch_namespace(
            &mut self.namespace,
            &program,
            &self.image,
            &self.log,
            boot_config(self.mounts),
        )
        .map_err(BootFailure::Namespace)
    }

    /// Снимает дерево имён и завершает маршрутизатор.
    pub fn shutdown(&mut self) {
        let Some(running) = self.namespace.running() else {
            return;
        };
        unmount_all(running.roots(), self.mounts, CALL_TIMEOUT);
        let _ = stop_namespace(&mut self.namespace, SHUTDOWN_EXIT_CODE, STOP_TIMEOUT);
    }
}

/// Поднимает систему и держит её до завершения критической подсистемы.
pub fn supervise(kernel: Handle) -> Stopped {
    let kernel_log = BootstrapClient::new(PortTransport::client(kernel));
    if relay::start(kernel).is_err() {
        // Отказ relay идёт в bootstrap-канал: по отказавшему пути диагностика
        // не проходит.
        let _ = kernel_log.log_str(LOG_TAG, "log relay not started");
        return Stopped::Startup;
    }
    let stopped = run(kernel, &kernel_log);
    relay::shutdown(STOP_TIMEOUT);
    stopped
}

/// Параметры managed-запуска маршрутизатора с деревом имён `mounts`.
#[must_use]
pub const fn boot_config<'a>(mounts: &'a [Mount<'a>]) -> NamespaceBootConfig<'a> {
    NamespaceBootConfig {
        budget_pages: ROUTER_BUDGET_PAGES,
        startup_timeout: CALL_TIMEOUT,
        admin_timeout: CALL_TIMEOUT,
        stop_timeout: STOP_TIMEOUT,
        mounts,
    }
}

fn run(kernel: Handle, kernel_log: &BootstrapClient<PortTransport>) -> Stopped {
    let Some(session) = relay::session(relay::SUPERVISOR_BADGE) else {
        let _ = kernel_log.log_str(LOG_TAG, "supervisor log session not minted");
        return Stopped::Startup;
    };
    let log = LogClient::new(PortTransport::client(&session));
    let _ = log.write_str(LOG_TAG, "started");

    let Some(router_log) = relay::delegated_session(relay::FILESYSTEM_BADGE) else {
        let _ = log.write_str(LOG_TAG, BootFailure::LogSession.message());
        return BootFailure::LogSession.stopped();
    };
    let Some(relay_thread) = relay::thread_handle() else {
        let _ = log.write_str(LOG_TAG, "log relay thread unavailable");
        return Stopped::Startup;
    };

    match RootSystem::bring_up(kernel, router_log, BOOT_MOUNTS) {
        Ok(mut system) => {
            let _ = log.write_str(LOG_TAG, "filesystem namespace ready");
            let stopped = loop {
                match system.supervise_turn(kernel, relay_thread) {
                    Supervised::Restarted { .. } => {
                        let _ = log.write_str(LOG_TAG, "filesystem namespace restarted");
                    }
                    Supervised::Stopped(stopped) => break stopped,
                }
            };
            system.shutdown();
            stopped
        }
        Err(failure) => {
            let _ = log.write_str(LOG_TAG, failure.message());
            failure.stopped()
        }
    }
}

fn acquire_image(kernel: Handle) -> Result<SystemImage, BootFailure> {
    let client = BootstrapClient::new(PortTransport::client(kernel));
    let cap = client
        .acquire_userland_image()
        .ok()
        .and_then(Result::ok)
        .ok_or(BootFailure::Image)?;
    let region = MemoryRegion::adopt(cap);
    let mapping = region
        .map_full_read_only()
        .map_err(|_| BootFailure::Image)?;
    Ok(SystemImage { region, mapping })
}

fn log_exit(kernel_log: &BootstrapClient<PortTransport>, subsystem: &str, exit_code: Option<u32>) {
    let mut message = LogBuffer::<EXIT_MESSAGE_MAX>::new();
    let written = match exit_code {
        Some(code) => write!(message, "{subsystem} stopped with exit code {code}"),
        None => write!(message, "{subsystem} stopped; exit code unavailable"),
    };
    if written.is_ok() {
        let _ = kernel_log.log_str(LOG_TAG, message.as_str());
    }
}
