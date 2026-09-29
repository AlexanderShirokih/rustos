//! Записи компонентов и обслуживание их требований родителем.

use alloc::{string::String, vec::Vec};
use core::{
    fmt::{Arguments, Write as _},
    ops::ControlFlow,
};

use bootstrap_abi::{ImageError, LOG_MESSAGE_MAX, bootstrap, hardware, image};
use component_abi::{
    AcceptError, ComponentManifest, LOG_REQUIREMENT, PROTOCOL_ID_MAX, REQUIREMENT_NAME_MAX,
    RequirementDeclaration, RequirementMode, SessionError, SessionRequest, SessionRequirement,
    component_startup, decode_manifest,
};
use ipc::{
    IntoWireValue, Listener as _, Protocol as _, RecvError,
    wire::{Cap, MESSAGE_INLINE_MAX, Str},
};
use kernel_objects::{
    Delivery, Error, IssuedHandle, MemoryRegion, Port, ReadOnlyMapping, Resource, Session, kind,
};
use log_abi::{LogBuffer, log};
use managed_component::{Children, Departure, GrantError, Outcome, Status};
use runtime::{Mutex, PortTransport, Timeout};
use syscall::{Handle, Rights};
use userland::{DecodedProgram, ProgramView};

use crate::component::{Component, ImporterRequirement, Record};

const LOG_TAG: &str = "rootkeeper";

const BOOT_RECORD: u16 = 0;

const IMAGE_REGION_GRANT: &str = "image region";

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

/// Отказ [`RootSystem::importer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImporterRefusal {
    /// Имя или протокол требования недопустимы.
    Requirement(usize),
    /// Порт импортёра не создан.
    Importer(Error),
}

#[derive(Debug)]
pub enum AddFailure {
    /// Имя записи уже зарегистрировано.
    Name,
    /// Число записей превышает диапазон номера компонента.
    Capacity,
    /// Описание требований, экспортов или бюджета недопустимо.
    Declaration,
    /// Резерв бюджета записи не создан.
    Resource(Error),
}

/// Userland-образ, отображённый в адресное пространство rootkeeper.
pub struct SystemImage {
    pub(crate) region: MemoryRegion,
    pub(crate) mapping: ReadOnlyMapping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Holder {
    Record(u16),
    Importer(usize),
    Image(u64),
}

struct ImporterBinding {
    name: String,
    protocol: String,
    mode: RequirementMode,
    provider: Option<u16>,
}

#[derive(Clone)]
struct Export<'a> {
    provider: u16,
    protocol: &'a str,
}

/// Родитель зарегистрированных компонентов. Поток обслуживает записи повтором [`turn`].
pub struct RootSystem<'a> {
    children: Children<'a, Holder, DecodedProgram<'a>>,
    records: Vec<Record<'a>>,
    logs: Vec<Session<log::Protocol>>,
    hardware: Session<hardware::Protocol>,
    image: &'a SystemImage,
    kernel: Handle<kind::Port>,
    restarts_used: Vec<u32>,
    importers: Vec<Vec<ImporterBinding>>,
    resource: Resource,
}

impl From<Stopped> for u64 {
    fn from(stopped: Stopped) -> Self {
        stopped as Self
    }
}

impl<'a> RootSystem<'a> {
    pub(crate) fn new(
        image: &'a SystemImage,
        kernel: Handle<kind::Port>,
        name: &'a str,
        manifest: ComponentManifest<'a>,
    ) -> Result<Self, BudgetFailure> {
        let resource = Resource::self_resource().map_err(|_| BudgetFailure::Root)?;
        let children = Children::new().map_err(|_| BudgetFailure::StartupPort)?;
        let hardware = bootstrap::Client::new(PortTransport::new(kernel))
            .acquire_hardware()
            .ok()
            .and_then(Result::ok)
            .map(Session::from_session)
            .ok_or(BudgetFailure::Hardware)?;

        Ok(Self {
            children,
            records: alloc::vec![Record {
                name,
                manifest,
                restarts: 0,
            }],
            logs: Vec::new(),
            hardware,
            image,
            kernel,
            restarts_used: alloc::vec![0],
            importers: Vec::new(),
            resource,
        })
    }

    /// Добавляет запись, которую запускает следующий оборот. Номера прежних записей сохраняются.
    pub fn add(
        &mut self,
        component: Component<'a>,
        log: Session<log::Protocol>,
    ) -> Result<u16, AddFailure> {
        if self.record_named(component.name.as_str()).is_some() {
            return Err(AddFailure::Name);
        }
        let manifest =
            decode_manifest(component.program.manifest()).map_err(|_| AddFailure::Declaration)?;
        if component.memory_limit == 0
            || manifest.requirements().any(|requirement| {
                requirement.name().len() > REQUIREMENT_NAME_MAX
                    || requirement.protocol().len() > PROTOCOL_ID_MAX
            })
            || manifest
                .exports()
                .any(|protocol| protocol.len() > PROTOCOL_ID_MAX)
        {
            return Err(AddFailure::Declaration);
        }

        let record = u16::try_from(self.records.len()).map_err(|_| AddFailure::Capacity)?;
        self.children
            .add(
                Holder::Record(record),
                component.program,
                component.name,
                &self.resource,
                component.memory_limit,
                BUDGET_RETURN_TIMEOUT,
            )
            .map_err(AddFailure::Resource)?;

        self.records.push(Record {
            name: component.name.as_str(),
            manifest,
            restarts: component.restarts,
        });
        self.logs.push(log);
        self.restarts_used.push(0);
        Ok(record)
    }

    /// Создаёт клиентскую capability импортёра с требованиями `requirements`. Родитель отвечает
    /// по ней только на запрос `session` требования из `requirements`. Поток, который зовёт
    /// [`turn`], не вызывает по ней.
    pub fn importer(
        &mut self,
        requirements: &[ImporterRequirement],
    ) -> Result<Port, ImporterRefusal> {
        let mut bindings: Vec<ImporterBinding> = Vec::with_capacity(requirements.len());
        for (index, requirement) in requirements.iter().enumerate() {
            let binding = Self::importer_binding(&requirement.declaration, &bindings)
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

    pub(crate) fn importer_for(
        &mut self,
        requirements: &[ImporterRequirement],
        requirement: &str,
        provider: &str,
    ) -> Result<Port, ImporterRefusal> {
        let (record, declared) = self
            .record_named(provider)
            .ok_or(ImporterRefusal::Requirement(0))?;
        let binding = requirements
            .iter()
            .position(|binding| binding.declaration.name() == requirement)
            .ok_or(ImporterRefusal::Requirement(0))?;
        if !declared.manifest.exports().any(|protocol| {
            requirements
                .get(binding)
                .is_some_and(|requirement| protocol == requirement.declaration.protocol())
        }) {
            return Err(ImporterRefusal::Requirement(binding));
        }
        let port = self.importer(requirements)?;
        if let Some(binding) = self
            .importers
            .last_mut()
            .and_then(|bindings| bindings.get_mut(binding))
        {
            binding.provider = Some(record);
        }
        Ok(port)
    }

    /// Завершает процесс работающей записи `name` кодом `code`. Следующий [`turn`] решает,
    /// что делать с записью.
    pub fn terminate(&mut self, name: &str, code: u32) {
        if let Some((record, _)) = self.record_named(name) {
            self.children.terminate(Holder::Record(record), code);
        }
    }

    fn record_named(&self, name: &str) -> Option<(u16, &Record<'a>)> {
        self.records
            .iter()
            .enumerate()
            .find(|(_, component)| component.name == name)
            .and_then(|(index, component)| Some((u16::try_from(index).ok()?, component)))
    }

    fn importer_binding(
        requirement: &RequirementDeclaration,
        bound: &[ImporterBinding],
    ) -> Option<ImporterBinding> {
        if requirement.name().is_empty()
            || requirement.name() == LOG_REQUIREMENT
            || requirement.name().len() > REQUIREMENT_NAME_MAX
            || bound
                .iter()
                .any(|binding| binding.name == requirement.name())
        {
            return None;
        }
        if requirement.protocol().is_empty() || requirement.protocol().len() > PROTOCOL_ID_MAX {
            return None;
        }
        Some(ImporterBinding {
            name: String::from(requirement.name()),
            protocol: String::from(requirement.protocol()),
            mode: requirement.mode(),
            provider: None,
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
        let Some(component) = self.records.get(usize::from(record)) else {
            return;
        };
        let name = component.name;
        let restarts = component.restarts;
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
                && *used < restarts
            {
                *used += 1;
                self.children.restart(child);
                return;
            }
            self.log(format_args!("{name} restart limit reached"));
        }
        self.children.remove(child);
    }

    fn record_export(
        &self,
        record: u16,
        requirement: &str,
        protocol: &str,
        index: u16,
    ) -> Result<Export<'a>, SessionError> {
        let requirement = self
            .records
            .get(usize::from(record))
            .and_then(|record| record.requirement(requirement))
            .ok_or(SessionError::Refused)?;
        if requirement.protocol() != protocol {
            return Err(SessionError::Refused);
        }
        self.resolve_export(protocol, requirement.mode(), index)
    }

    fn resolve_export(
        &self,
        protocol: &str,
        mode: RequirementMode,
        index: u16,
    ) -> Result<Export<'a>, SessionError> {
        let provider = if mode == RequirementMode::All {
            self.providers(protocol)
                .nth(usize::from(index))
                .ok_or(SessionError::Unbound)?
        } else {
            if index != 0 {
                return Err(SessionError::Unbound);
            }
            let mut providers = self.providers(protocol).filter(|provider| {
                *provider == BOOT_RECORD
                    || !matches!(
                        self.children.status(Holder::Record(*provider)),
                        None | Some(Status::Removed)
                    )
            });
            let provider = providers.next().ok_or(SessionError::Unbound)?;
            if providers.next().is_some() {
                return Err(SessionError::Refused);
            }
            provider
        };
        let protocol = self
            .records
            .get(usize::from(provider))
            .and_then(|record| record.manifest.exports().find(|export| *export == protocol))
            .ok_or(SessionError::Refused)?;
        Ok(Export { provider, protocol })
    }

    fn providers<'s>(&'s self, protocol: &'s str) -> impl Iterator<Item = u16> + 's {
        self.records
            .iter()
            .enumerate()
            .filter(move |(_, record)| record.manifest.exports().any(|export| export == protocol))
            .filter_map(|(index, _)| u16::try_from(index).ok())
    }

    fn importer_export(
        &self,
        importer: usize,
        requirement: &str,
        protocol: &str,
        index: u16,
    ) -> Result<Export<'a>, SessionError> {
        let binding = self
            .importers
            .get(importer)
            .and_then(|requirements| {
                requirements
                    .iter()
                    .find(|binding| binding.name == requirement && binding.protocol == protocol)
            })
            .ok_or(SessionError::Refused)?;
        if let Some(provider) = binding.provider {
            if index != 0 {
                return Err(SessionError::Unbound);
            }
            let protocol = self
                .records
                .get(usize::from(provider))
                .and_then(|record| record.manifest.exports().find(|export| *export == protocol))
                .ok_or(SessionError::Refused)?;
            return Ok(Export { provider, protocol });
        }
        self.resolve_export(protocol, binding.mode, index)
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
        let session = match protocol {
            hardware::Protocol::ID => self.children.grant(
                badge,
                requirement,
                self.hardware.handle().borrow(),
                Rights::WRITE | Rights::TRANSFER,
            ),
            image::Protocol::ID => {
                self.children
                    .grant_importer(badge, requirement, Holder::Image(badge))
            }
            _ => return Err(SessionError::Refused),
        };
        session.map_err(SessionError::from)
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
        let index = requirement.index;
        let export = match self.children.caller(badge) {
            Some(Holder::Record(record)) if name == LOG_REQUIREMENT => {
                let answer = if index == 0 {
                    self.log_session(record, badge, protocol)
                } else {
                    Err(SessionError::Unbound)
                };
                let _ = reply.send(answer);
                return;
            }
            Some(Holder::Record(record)) => self.record_export(record, name, protocol, index),
            Some(Holder::Importer(importer)) => {
                self.importer_export(importer, name, protocol, index)
            }
            Some(Holder::Image(_)) | None => Err(SessionError::Refused),
        };
        let export = match export {
            Ok(export) => export,
            Err(error) => {
                let _ = reply.send(Err::<Port, _>(error));
                return;
            }
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
        self.children.request(provider, badge, requirement, reply);
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
                    .records
                    .get(usize::from(record))
                    .is_some_and(|component| {
                        component
                            .manifest
                            .exports()
                            .any(|protocol| protocol == export)
                    }) =>
            {
                Holder::Record(record)
            }
            Some(Holder::Record(_) | Holder::Importer(_) | Holder::Image(_)) | None => {
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

impl image::Service<Delivery> for RootSystem<'_> {
    fn region(
        &mut self,
        frame: &Delivery,
    ) -> impl IntoWireValue<Delivery, Read = Result<Cap<kind::MemoryRegion>, ImageError>> {
        let Some(Holder::Image(incarnation)) = self.children.caller(frame.badge) else {
            return Err(ImageError::Refused);
        };
        // Ключ выдачи содержит пробел, которого нет в имени требования.
        self.children
            .grant(
                incarnation,
                IMAGE_REGION_GRANT,
                self.image.region.borrow(),
                Rights::READ | Rights::DUPLICATE | Rights::TRANSFER,
            )
            .map_err(|error| match error {
                GrantError::NotRunning => ImageError::Refused,
                GrantError::Kernel(_) => ImageError::NoSpace,
            })
    }
}

/// Обслуживает один кадр композиции `system`, ожидая его не дольше `timeout`. На время ожидания
/// замок `system` свободен. `Break` - порт закрыт.
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
            let mut guard = system.lock();
            // `dispatch` забирает кадр и разрушает кадр чужой операции, поэтому протокол
            // выбирается по значку до разбора.
            let _ = match guard.children.caller(incoming.resources.badge) {
                Some(Holder::Image(_)) => image::dispatch(&mut *guard, incoming),
                _ => component_startup::dispatch(&mut *guard, incoming),
            };
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

pub(crate) fn log_line(kernel: Handle<kind::Port>, line: Arguments<'_>) {
    let mut message = LogBuffer::<LOG_MESSAGE_MAX>::new();
    let _ = message.write_fmt(line);
    let _ = bootstrap::Client::new(PortTransport::new(kernel)).log_str(LOG_TAG, message.as_str());
}
