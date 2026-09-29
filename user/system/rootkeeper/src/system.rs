//! Композиция, которую rootkeeper поднимает при загрузке, и её родитель.

use alloc::{format, string::String, vec, vec::Vec};
use core::{
    fmt::{Arguments, Write as _},
    num::NonZeroU64,
    ops::ControlFlow,
};

use bootstrap_abi::{LOG_MESSAGE_MAX, bootstrap, hardware};
use component_abi::{
    AcceptError, LOG_REQUIREMENT, PROTOCOL_ID_MAX, REQUIREMENT_NAME_MAX, ResourceError,
    SessionError, SessionRequest, SessionRequirement, SessionScope, component_startup,
};
use ipc::{
    Listener as _, Protocol as _, RecvError,
    wire::{MESSAGE_INLINE_MAX, Str},
};
use kernel_objects::{
    Delivery, Error, IssuedHandle, MemoryRegion, Port, ReadOnlyMapping, Resource, Session, kind,
};
use log_abi::{LogBuffer, log};
use log_relay::{KernelLogSink, LogRelayThread};
use managed_component::{Children, Departure, Outcome, Status};
use runtime::{Mutex, PortTransport, Timeout};
use syscall::{Handle, Rights};
use userland_image::{
    BOOT_PLAN_PATH, BootPlan, COMPONENT_PROGRAM_NAME, DecodedImage, DecodedProgram, PlanComponent,
    PlanDecodeError, RequirementSource, decode, decode_plan,
};

use crate::relay::{ComponentNames, log_badge};

const LOG_TAG: &str = "rootkeeper";

const BOOT_RECORD: u16 = 0;

/// Срок возврата бюджета перед перезапуском записи.
pub const BUDGET_RETURN_TIMEOUT: Timeout = Timeout::from_secs(5);

type Debt = IssuedHandle<kind::Reply>;

/// Код, с которым завершается процесс rootkeeper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stopped {
    /// Relay журнала не поднялся либо ресурса композиции не хватило.
    Startup = 1,
    /// Userland-образ, план загрузки либо программа записи недоступны.
    BadImage = 2,
    /// Главный поток вышел из оборота либо паниковал.
    Subsystem = 3,
}

/// Причина, по которой композиция не поднялась.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootFailure {
    /// Userland-образ недоступен либо не разобран.
    Image,
    /// В образе нет записи плана загрузочной композиции.
    PlanMissing,
    /// Запись плана не разобрана.
    Plan(PlanDecodeError),
    /// В образе нет программы записи плана.
    ProgramMissing,
    /// Ресурса композиции не хватило.
    Budget(BudgetFailure),
}

/// Ресурс композиции, которого не хватило.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetFailure {
    /// Сессия журнала записи не выдана.
    LogSession,
    /// Корневой metering resource недоступен.
    Root,
    /// Резерв бюджета компонента не выдан.
    Reserve,
    /// Порт `ComponentStartup` не создан.
    StartupPort,
    /// Ядро не выдало сессию `Hardware`.
    Hardware,
}

/// Требование `name` импортёра. Импортёр получает по нему сессию экспорта `protocol` записи
/// плана `provider` с областью `scope`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImporterRequirement<'n> {
    pub name: &'n str,
    pub provider: &'n str,
    pub protocol: &'n str,
    pub scope: &'n str,
}

/// Отказ [`RootSystem::importer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImporterRefusal {
    /// Требование с этим номером не связывается с экспортом плана.
    Requirement(usize),
    /// Порт импортёра не создан.
    Importer(Error),
}

/// Userland-образ, отображённый в адресное пространство rootkeeper.
pub struct SystemImage {
    region: MemoryRegion,
    mapping: ReadOnlyMapping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Holder {
    Record(u16),
    Importer(usize),
}

struct ImporterBinding<'a> {
    name: String,
    export: Export<'a>,
}

#[derive(Clone)]
struct Export<'a> {
    provider: u16,
    protocol: &'a str,
    scope: SessionScope,
}

/// Композиция плана и её родитель. Поток rootkeeper обслуживает композицию повтором [`turn`].
pub struct RootSystem<'a> {
    children: Children<'a, Holder, DecodedProgram<'a>>,
    plan: BootPlan<'a>,
    logs: Vec<Session<log::Protocol>>,
    hardware: Session<hardware::Protocol>,
    image: &'a SystemImage,
    kernel: Handle<kind::Port>,
    restarts_used: Vec<u32>,
    importers: Vec<Vec<ImporterBinding<'a>>>,
    _resource: Resource,
}

impl From<Stopped> for u64 {
    fn from(stopped: Stopped) -> Self {
        stopped as Self
    }
}

impl BootFailure {
    /// Код выхода rootkeeper, соответствующий отказу.
    #[must_use]
    pub const fn stopped(self) -> Stopped {
        match self {
            Self::Image | Self::PlanMissing | Self::Plan(_) | Self::ProgramMissing => {
                Stopped::BadImage
            }
            Self::Budget(_) => Stopped::Startup,
        }
    }

    const fn message(self) -> &'static str {
        match self {
            Self::Image => "userland image unavailable",
            Self::PlanMissing => "boot plan record missing",
            Self::Plan(_) => "boot plan not decoded",
            Self::ProgramMissing => "component program missing",
            Self::Budget(BudgetFailure::LogSession) => "log session not minted",
            Self::Budget(BudgetFailure::Root) => "root resource unavailable",
            Self::Budget(BudgetFailure::Reserve) => "component reserve not granted",
            Self::Budget(BudgetFailure::StartupPort) => "startup port not created",
            Self::Budget(BudgetFailure::Hardware) => "hardware session not granted",
        }
    }
}

impl SystemImage {
    /// Забирает у ядра capability образа и отображает его read-only.
    pub fn acquire(kernel: Handle<kind::Port>) -> Result<Self, BootFailure> {
        let client = bootstrap::Client::new(PortTransport::new(kernel));
        let region = client
            .acquire_userland_image()
            .map_err(|_| BootFailure::Image)?
            .map(MemoryRegion::from)
            .map_err(|_| BootFailure::Image)?;
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
}

impl<'a> RootSystem<'a> {
    /// Заводит записи плана `plan` с номера 1, которые запускает первый [`turn`]. Запись 0 -
    /// программа самого rootkeeper, и её он не запускает. `log_sessions` создаёт источник
    /// журнала записи по значку [`log_badge`](crate::relay::log_badge) её номера.
    pub fn bring_up(
        image: &'a SystemImage,
        plan: BootPlan<'a>,
        kernel: Handle<kind::Port>,
        log_sessions: impl Fn(NonZeroU64) -> Option<Session<log::Protocol>>,
    ) -> Result<Self, BootFailure> {
        let entries = image.entries()?;
        let resource =
            Resource::self_resource().map_err(|_| BootFailure::Budget(BudgetFailure::Root))?;
        let mut children =
            Children::new().map_err(|_| BootFailure::Budget(BudgetFailure::StartupPort))?;
        let logs = (BOOT_RECORD + 1..plan.len())
            .map(|record| log_sessions(log_badge(record)))
            .collect::<Option<Vec<_>>>()
            .ok_or(BootFailure::Budget(BudgetFailure::LogSession))?;
        let hardware = bootstrap::Client::new(PortTransport::new(kernel))
            .acquire_hardware()
            .ok()
            .and_then(Result::ok)
            .map(Session::from_session)
            .ok_or(BootFailure::Budget(BudgetFailure::Hardware))?;

        for (record, component) in (0..plan.len())
            .zip(plan.components())
            .skip(usize::from(BOOT_RECORD) + 1)
        {
            let path = format!("{}/{COMPONENT_PROGRAM_NAME}", component.executable());
            let program = entries
                .entry(&path)
                .and_then(|entry| entry.program().ok())
                .ok_or(BootFailure::ProgramMissing)?;
            children
                .add(
                    Holder::Record(record),
                    program,
                    component.name(),
                    &resource,
                    component.memory_limit(),
                    BUDGET_RETURN_TIMEOUT,
                )
                .map_err(|_| BootFailure::Budget(BudgetFailure::Reserve))?;
        }

        Ok(Self {
            children,
            plan,
            logs,
            hardware,
            image,
            kernel,
            restarts_used: vec![0; usize::from(plan.len())],
            importers: Vec::new(),
            _resource: resource,
        })
    }

    /// Создаёт клиентскую capability импортёра с требованиями `requirements`. Родитель отвечает
    /// по ней только на запрос `session` требования из `requirements`. Поток, который зовёт
    /// [`turn`], не вызывает по ней.
    pub fn importer(
        &mut self,
        requirements: &[ImporterRequirement<'_>],
    ) -> Result<Port, ImporterRefusal> {
        let mut bindings: Vec<ImporterBinding<'a>> = Vec::with_capacity(requirements.len());
        for (index, requirement) in requirements.iter().enumerate() {
            let binding = self
                .importer_binding(requirement, &bindings)
                .ok_or(ImporterRefusal::Requirement(index))?;
            bindings.push(binding);
        }
        let port = self
            .children
            .importer(Holder::Importer(self.importers.len()))
            .map_err(ImporterRefusal::Importer)?;
        self.importers.push(bindings);
        Ok(port)
    }

    /// Завершает процесс работающей записи плана `name` кодом `code`. Следующий [`turn`] решает,
    /// что делать с записью.
    pub fn terminate(&mut self, name: &str, code: u32) {
        if let Some((record, _)) = self.record_named(name) {
            self.children.terminate(Holder::Record(record), code);
        }
    }

    fn record_named(&self, name: &str) -> Option<(u16, PlanComponent<'a>)> {
        (0..self.plan.len())
            .zip(self.plan.components())
            .find(|(_, component)| component.name() == name)
    }

    fn importer_binding(
        &self,
        requirement: &ImporterRequirement<'_>,
        bound: &[ImporterBinding<'a>],
    ) -> Option<ImporterBinding<'a>> {
        if requirement.name == LOG_REQUIREMENT
            || requirement.name.len() > REQUIREMENT_NAME_MAX
            || bound.iter().any(|binding| binding.name == requirement.name)
        {
            return None;
        }
        let (provider, component) = self.record_named(requirement.provider)?;
        let protocol = component
            .exports()
            .find(|export| *export == requirement.protocol)?;
        Some(ImporterBinding {
            name: String::from(requirement.name),
            export: Export {
                provider,
                protocol,
                scope: SessionScope::new(requirement.scope)?,
            },
        })
    }

    fn launch_due(&mut self) {
        while let Some(departure) = self.children.launch_due() {
            self.decide(departure);
        }
    }

    fn decide(&mut self, departure: Departure<Holder>) {
        let Holder::Record(record) = departure.child else {
            return;
        };
        let Some(component) = self.plan.component(record) else {
            return;
        };
        let name = component.name();
        let retry = match departure.outcome {
            Outcome::Exit(code) => {
                self.log(format_args!("{name} stopped with exit code {code}"));
                code != 0
            }
            Outcome::TakenDown => {
                self.log(format_args!("{name} taken down: startup session closed"));
                true
            }
            Outcome::LaunchFailed => {
                self.log(format_args!("{name} launch failed"));
                false
            }
            Outcome::BudgetNotReturned => {
                self.log(format_args!("{name} budget not returned"));
                false
            }
        };

        let child = Holder::Record(record);
        if retry {
            if let Some(used) = self.restarts_used.get_mut(usize::from(record))
                && *used < component.restarts()
            {
                *used += 1;
                self.children.restart(child);
                return;
            }
            self.log(format_args!("{name} restart limit reached"));
        }
        self.children.remove(child);
    }

    fn record_export(&self, record: u16, requirement: &str) -> Option<Export<'a>> {
        match self.plan.component(record)?.requirement(requirement)? {
            RequirementSource::Export {
                provider,
                protocol,
                scope,
            } => Some(Export {
                provider,
                protocol,
                scope: SessionScope::new(scope)?,
            }),
            RequirementSource::Region => None,
        }
    }

    fn importer_export(&self, importer: usize, requirement: &str) -> Option<Export<'a>> {
        self.importers
            .get(importer)?
            .iter()
            .find(|binding| binding.name == requirement)
            .map(|binding| binding.export.clone())
    }

    fn log_session(
        &mut self,
        record: u16,
        badge: u64,
        protocol: &str,
    ) -> Result<Port, SessionError> {
        if protocol != log::Protocol::ID {
            return Err(SessionError::Refused);
        }
        let source = record
            .checked_sub(BOOT_RECORD + 1)
            .and_then(|index| self.logs.get(usize::from(index)))
            .ok_or(SessionError::Refused)?;
        self.children
            .grant(
                badge,
                LOG_REQUIREMENT,
                source.handle().borrow(),
                Rights::WRITE | Rights::TRANSFER,
            )
            .map_err(SessionError::from)
    }

    fn boot_export(
        &mut self,
        badge: u64,
        requirement: &str,
        protocol: &str,
    ) -> Result<Port, SessionError> {
        if protocol != hardware::Protocol::ID {
            return Err(SessionError::Refused);
        }
        self.children
            .grant(
                badge,
                requirement,
                self.hardware.handle().borrow(),
                Rights::WRITE | Rights::TRANSFER,
            )
            .map_err(SessionError::from)
    }

    fn record_requirement(
        &self,
        badge: u64,
        requirement: &str,
    ) -> Option<(u16, RequirementSource<'a>)> {
        let Holder::Record(record) = self.children.caller(badge)? else {
            return None;
        };
        Some((
            record,
            self.plan.component(record)?.requirement(requirement)?,
        ))
    }

    fn log(&self, line: Arguments<'_>) {
        log_line(self.kernel, line);
    }
}

impl component_startup::Service<Delivery> for RootSystem<'_> {
    fn session(
        &mut self,
        frame: &Delivery,
        requirement: SessionRequirement<'_>,
        reply: component_startup::session::Reply<Debt>,
    ) {
        let badge = frame.badge;
        let name = requirement.name.as_str();
        let protocol = requirement.protocol.as_str();
        let export = match self.children.caller(badge) {
            Some(Holder::Record(record)) if name == LOG_REQUIREMENT => {
                let answer = self.log_session(record, badge, protocol);
                let _ = reply.send(answer);
                return;
            }
            Some(Holder::Record(record)) => self.record_export(record, name),
            Some(Holder::Importer(importer)) => self.importer_export(importer, name),
            None => None,
        };
        let Some(export) = export.filter(|export| export.protocol == protocol) else {
            let _ = reply.send(Err::<Port, _>(SessionError::Refused));
            return;
        };
        if export.provider == BOOT_RECORD {
            let answer = self.boot_export(badge, name, export.protocol);
            let _ = reply.send(answer);
            return;
        }

        let provider = Holder::Record(export.provider);
        if matches!(self.children.status(provider), None | Some(Status::Removed)) {
            let _ = reply.send(Err::<Port, _>(SessionError::Unavailable));
            return;
        }
        self.children
            .request(provider, export.protocol, badge, name, export.scope, reply);
    }

    fn region(
        &mut self,
        frame: &Delivery,
        requirement: Str<'_, { REQUIREMENT_NAME_MAX }>,
        reply: component_startup::region::Reply<Debt>,
    ) {
        let requirement = requirement.as_str();
        let answer = match self.record_requirement(frame.badge, requirement) {
            Some((_, RequirementSource::Region)) => self
                .children
                .grant(
                    frame.badge,
                    requirement,
                    self.image.region.borrow(),
                    Rights::READ | Rights::DUPLICATE | Rights::TRANSFER,
                )
                .map_err(ResourceError::from),
            _ => Err(ResourceError::Refused),
        };
        let _ = reply.send(answer);
    }

    fn accept(
        &mut self,
        frame: &Delivery,
        export: Str<'_, { PROTOCOL_ID_MAX }>,
        reply: component_startup::accept::Reply<Debt>,
    ) {
        let export = export.as_str();
        let provider = match self.children.caller(frame.badge) {
            Some(Holder::Record(record))
                if self
                    .plan
                    .component(record)
                    .is_some_and(|component| component.exports().any(|name| name == export)) =>
            {
                Holder::Record(record)
            }
            Some(Holder::Record(_) | Holder::Importer(_)) | None => {
                let _ = reply.send(Err::<
                    SessionRequest<component_startup::session::Reply<Debt>>,
                    _,
                >(AcceptError::Refused));
                return;
            }
        };
        self.children.accept(provider, frame.badge, export, reply);
    }
}

/// Один оборот обслуживания композиции `system`: запуск ждущих записей с номера 1, приём кадра
/// не дольше `timeout` и его обслуживание. Запрос к экспорту записи 0 обслуживает сам оборот.
/// На время приёма замок `system` отпущен. `Break` означает, что порт закрыт.
pub fn turn(system: &Mutex<RootSystem<'_>>, timeout: Timeout) -> ControlFlow<()> {
    let (port, wait) = {
        let mut guard = system.lock();
        guard.launch_due();
        let next = guard.children.next_launch();
        let wait = if next.raw() < timeout.raw() {
            next
        } else {
            timeout
        };
        (guard.children.port().as_raw(), wait)
    };

    let mut bytes = [0u8; MESSAGE_INLINE_MAX];
    match PortTransport::new(port).recv(&mut bytes, wait) {
        Ok(incoming) => {
            let _ = component_startup::dispatch(&mut *system.lock(), incoming);
        }
        Err(RecvError::SenderGone(badge)) => {
            let mut guard = system.lock();
            if let Some(departure) = guard.children.depart(badge) {
                guard.decide(departure);
            }
        }
        Err(RecvError::Timeout | RecvError::Discarded) => {}
        Err(RecvError::Closed) => return ControlFlow::Break(()),
    }
    ControlFlow::Continue(())
}

/// Поднимает композицию загрузочного плана и служит её родителем. Возвращается только при
/// отказе подъёма либо выходе из оборота.
pub fn run(kernel: Handle<kind::Port>) -> Stopped {
    let image = match SystemImage::acquire(kernel) {
        Ok(image) => image,
        Err(failure) => return refuse_boot(kernel, failure),
    };
    let plan = match image.plan(BOOT_PLAN_PATH) {
        Ok(plan) => plan,
        Err(failure) => return refuse_boot(kernel, failure),
    };
    let Ok(relay) = LogRelayThread::start(ComponentNames::of(&plan), KernelLogSink::new(kernel))
    else {
        log_line(kernel, format_args!("log relay not started"));
        return Stopped::Startup;
    };
    let system = match RootSystem::bring_up(&image, plan, kernel, |badge| relay.session(badge).ok())
    {
        Ok(system) => Mutex::new(system),
        Err(failure) => return refuse_boot(kernel, failure),
    };
    log_line(kernel, format_args!("composition started"));

    while turn(&system, Timeout::INFINITE).is_continue() {}
    log_line(kernel, format_args!("startup port closed"));
    Stopped::Subsystem
}

fn refuse_boot(kernel: Handle<kind::Port>, failure: BootFailure) -> Stopped {
    log_line(kernel, format_args!("{}", failure.message()));
    failure.stopped()
}

fn log_line(kernel: Handle<kind::Port>, line: Arguments<'_>) {
    let mut message = LogBuffer::<LOG_MESSAGE_MAX>::new();
    let _ = message.write_fmt(line);
    let _ = bootstrap::Client::new(PortTransport::new(kernel)).log_str(LOG_TAG, message.as_str());
}
