//! Композиция, которую rootkeeper поднимает при загрузке, и её supervisor.

use core::fmt::Write as _;

use bootstrap::BootstrapClient;
use log_abi::{LogBuffer, LogClient};
use managed_component::{ComponentRoots, ManagedComponent, ManagedStartupError, launch_component};
use runtime::{
    MemoryRegion, OwnedHandle, PortTransport, ReadOnlyMapping, Resource, Timeout, wait_any,
};
use syscall::{Handle, SIGNALED, WaitItem};
use userland_image::{
    BOOT_PLAN_PATH, BootPlan, DecodedImage, DecodedProgram, PlanComponent, PlanDecodeError, decode,
    decode_plan,
};

use crate::relay;

// Тег строк supervisor в журнале.
const LOG_TAG: &str = "supervisor";

/// Конечный срок остановки подсистемы: terminate маршрутизатора тянет за собой
/// join каждого его монтирования, и каждое отвечает в пределах admin-срока.
pub const STOP_TIMEOUT: Timeout = Timeout::from_secs(5);

/// Исход прохода supervisor: этим кодом ядро гасит машину.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stopped {
    /// Инфраструктура supervisor не поднялась.
    Startup = 1,
    /// Userland-образ либо план загрузки недоступны.
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
    /// В образе нет записи плана загрузочной композиции.
    PlanMissing,
    /// Запись плана не разобрана.
    Plan(PlanDecodeError),
    /// В образе нет программы компонента.
    ProgramMissing,
    /// Корневой metering resource недоступен.
    Resource,
    /// Managed-запуск компонента не завершился.
    Launch(ManagedStartupError),
}

/// Исход одного оборота supervisor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Supervised {
    /// Компонент ушёл с кодом `exit_code`, и композиция подняла его новую
    /// incarnation.
    Restarted { exit_code: Option<u32> },
    /// Некритический компонент исчерпал запуски и снят с композиции, а сама
    /// композиция осталась поднятой.
    Abandoned { exit_code: Option<u32> },
    /// Композиция остановлена: восстановить её оборот supervisor не смог.
    Stopped(Stopped),
}

/// Userland-образ, отображённый в адресное пространство rootkeeper.
pub struct SystemImage {
    region: MemoryRegion,
    mapping: ReadOnlyMapping,
}

/// Поднятая композиция rootkeeper.
pub struct RootSystem<'a> {
    managed: ManagedComponent<ComponentRoots>,
    plan: PlanComponent<'a>,
    _resource: Resource,
    image: &'a SystemImage,
    entries: DecodedImage<'a>,
    log: OwnedHandle,
}

const SHUTDOWN_EXIT_CODE: u32 = 1;
const EXIT_MESSAGE_MAX: usize = 96;

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
            Self::Image | Self::PlanMissing | Self::Plan(_) | Self::ProgramMissing => {
                Stopped::BadImage
            }
            Self::LogSession | Self::Resource | Self::Launch(_) => Stopped::Startup,
        }
    }

    /// Строка отказа для журнала.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::LogSession => "log session not minted",
            Self::Image => "userland image unavailable",
            Self::PlanMissing => "boot plan record missing",
            Self::Plan(_) => "boot plan not decoded",
            Self::ProgramMissing => "component program missing",
            Self::Resource => "root resource unavailable",
            Self::Launch(_) => "component not started",
        }
    }
}

impl SystemImage {
    /// Забирает у ядра capability образа и отображает его read-only.
    pub fn acquire(kernel: Handle) -> Result<Self, BootFailure> {
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
        Ok(Self { region, mapping })
    }

    /// Таблица записей образа.
    pub fn entries(&self) -> Result<DecodedImage<'_>, BootFailure> {
        decode(self.mapping.as_bytes()).map_err(|_| BootFailure::Image)
    }

    /// Разбирает план загрузочной композиции из записи `entry`.
    pub fn plan(&self, entry: &str) -> Result<BootPlan<'_>, BootFailure> {
        let record = self
            .entries()?
            .entry(entry)
            .ok_or(BootFailure::PlanMissing)?;
        decode_plan(record.data()).map_err(BootFailure::Plan)
    }

    /// Регион образа, из которого нарезаются производные капы incarnation.
    #[must_use]
    pub const fn region(&self) -> &MemoryRegion {
        &self.region
    }
}

impl<'a> RootSystem<'a> {
    /// Поднимает компонент плана `plan`.
    pub fn bring_up(
        image: &'a SystemImage,
        plan: PlanComponent<'a>,
        log: OwnedHandle,
    ) -> Result<Self, BootFailure> {
        let entries = image.entries()?;
        let program = component_program(&entries, plan)?;
        let resource = Resource::self_resource().map_err(|_| BootFailure::Resource)?;
        let mut managed =
            ManagedComponent::new(&resource, plan.budget_pages()).map_err(BootFailure::Launch)?;
        launch(&mut managed, &program, image.region(), &log, plan)?;

        Ok(Self {
            managed,
            plan,
            _resource: resource,
            image,
            entries,
            log,
        })
    }

    /// Managed-компонент композиции.
    #[must_use]
    pub const fn managed(&self) -> &ManagedComponent<ComponentRoots> {
        &self.managed
    }

    /// Оборот supervisor: ждёт завершения компонента либо потока
    /// `relay_thread`, записывает исход в журнал ядра и поднимает компоненту
    /// смену.
    ///
    /// Завершение потока пересылки критично: восстановить его композиция не
    /// умеет.
    pub fn supervise_turn(&mut self, kernel: Handle, relay_thread: Handle) -> Supervised {
        let kernel_log = BootstrapClient::new(PortTransport::client(kernel));
        let relay_wait = WaitItem::new(relay_thread, SIGNALED);
        let component = self
            .managed
            .running()
            .map(|running| WaitItem::new(running.process().handle().as_raw(), SIGNALED));
        // Снятый с композиции компонент ожидания не занимает: живым остаётся
        // только поток пересылки.
        let (waits, count) = match component {
            Some(component) => ([component, relay_wait], 2),
            None => ([relay_wait, relay_wait], 1),
        };
        match wait_any(&waits[..count], Timeout::INFINITE) {
            Ok(0) if count == 2 => self.replace_component(&kernel_log),
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

    /// Завершает компонент композиции.
    pub fn shutdown(&mut self) {
        let _ = self.managed.stop(SHUTDOWN_EXIT_CODE, STOP_TIMEOUT);
    }

    fn replace_component(&mut self, kernel_log: &BootstrapClient<PortTransport>) -> Supervised {
        let exit_code = self
            .managed
            .running()
            .and_then(|running| running.process().exit_code().ok());
        let observed = self.managed.observe_exit(Timeout::POLL);
        log_exit(kernel_log, self.plan.id(), exit_code);
        if observed.is_err() {
            let _ = kernel_log.log_str(LOG_TAG, "component budget not returned");
            return Supervised::Stopped(Stopped::Subsystem);
        }
        if self.managed.generation() >= self.plan.restart_limit() {
            let _ = kernel_log.log_str(LOG_TAG, "component restart limit reached");
            return self.give_up(exit_code);
        }
        if self.restart().is_err() {
            let _ = kernel_log.log_str(LOG_TAG, "component not restarted");
            return self.give_up(exit_code);
        }
        Supervised::Restarted { exit_code }
    }

    const fn give_up(&self, exit_code: Option<u32>) -> Supervised {
        // Уход критического компонента останавливает композицию, некритический
        // оставляет её поднятой без него.
        if self.plan.critical() {
            Supervised::Stopped(Stopped::Subsystem)
        } else {
            Supervised::Abandoned { exit_code }
        }
    }

    fn restart(&mut self) -> Result<(), BootFailure> {
        let program = component_program(&self.entries, self.plan)?;
        launch(
            &mut self.managed,
            &program,
            self.image.region(),
            &self.log,
            self.plan,
        )
    }
}

/// Поднимает систему и держит её до завершения критической подсистемы.
pub fn supervise(kernel: Handle) -> Stopped {
    let kernel_log = BootstrapClient::new(PortTransport::client(kernel));
    if relay::start(kernel).is_err() {
        // Отказ relay идёт в bootstrap-канал: по отказавшему пути диагностика не проходит.
        let _ = kernel_log.log_str(LOG_TAG, "log relay not started");
        return Stopped::Startup;
    }
    let stopped = run(kernel, &kernel_log);
    relay::shutdown(STOP_TIMEOUT);
    stopped
}

fn run(kernel: Handle, kernel_log: &BootstrapClient<PortTransport>) -> Stopped {
    let Some(session) = relay::session(relay::SUPERVISOR_BADGE) else {
        let _ = kernel_log.log_str(LOG_TAG, "supervisor log session not minted");
        return Stopped::Startup;
    };
    let log = LogClient::new(PortTransport::client(&session));
    let _ = log.write_str(LOG_TAG, "started");

    let Some(component_log) = relay::delegated_session(relay::FILESYSTEM_BADGE) else {
        let _ = log.write_str(LOG_TAG, BootFailure::LogSession.message());
        return BootFailure::LogSession.stopped();
    };
    let Some(relay_thread) = relay::thread_handle() else {
        let _ = log.write_str(LOG_TAG, "log relay thread unavailable");
        return Stopped::Startup;
    };

    let image = match SystemImage::acquire(kernel) {
        Ok(image) => image,
        Err(failure) => {
            let _ = log.write_str(LOG_TAG, failure.message());
            return failure.stopped();
        }
    };
    let plan = match image.plan(BOOT_PLAN_PATH) {
        Ok(plan) => plan,
        Err(failure) => {
            let _ = log.write_str(LOG_TAG, failure.message());
            return failure.stopped();
        }
    };

    run_component(
        RunningComposition {
            image: &image,
            plan: plan.component(),
            component_log,
            kernel,
            relay_thread,
        },
        &log,
    )
}

fn launch(
    managed: &mut ManagedComponent<ComponentRoots>,
    program: &DecodedProgram<'_>,
    image: &MemoryRegion,
    log: &OwnedHandle,
    plan: PlanComponent<'_>,
) -> Result<(), BootFailure> {
    launch_component(
        managed,
        program,
        image,
        log,
        plan.startup_timeout(),
        plan.config(),
    )
    .map_err(BootFailure::Launch)
}

struct RunningComposition<'a> {
    image: &'a SystemImage,
    plan: PlanComponent<'a>,
    component_log: OwnedHandle,
    kernel: Handle,
    relay_thread: Handle,
}

fn run_component(composition: RunningComposition<'_>, log: &LogClient<PortTransport>) -> Stopped {
    let RunningComposition {
        image,
        plan,
        component_log,
        kernel,
        relay_thread,
    } = composition;
    match RootSystem::bring_up(image, plan, component_log) {
        Ok(mut system) => {
            let _ = log.write_str(LOG_TAG, "component started");
            let stopped = loop {
                match system.supervise_turn(kernel, relay_thread) {
                    Supervised::Restarted { .. } => {
                        let _ = log.write_str(LOG_TAG, "component restarted");
                    }
                    Supervised::Abandoned { .. } => {
                        let _ = log.write_str(LOG_TAG, "component abandoned");
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

fn component_program<'a>(
    entries: &DecodedImage<'a>,
    plan: PlanComponent<'a>,
) -> Result<DecodedProgram<'a>, BootFailure> {
    entries
        .entry(plan.program())
        .and_then(|entry| entry.program().ok())
        .ok_or(BootFailure::ProgramMissing)
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
