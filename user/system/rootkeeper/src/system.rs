//! Записи компонентов и обслуживание их требований родителем.

use alloc::{collections::VecDeque, string::String, vec::Vec};
use core::{
    fmt::{Arguments, Write as _},
    ops::ControlFlow,
};

use bootstrap_abi::{
    FEATURE_MAX_BYTES, FeaturesError, ImageError, bootstrap, features, hardware, image,
    log_message_max,
};
use component_abi::{
    AcceptError, AttributeSet, Attributes, ComponentManifest, Found, LOG_REQUIREMENT,
    ManifestExport, PROTOCOL_ID_MAX, ProtocolName, QueryAttributes, REQUIREMENT_NAME_MAX,
    RequirementDeclaration, RequirementMode, SessionError, SessionRequest, SessionRequirement,
    component_startup, decode_manifest, match_filters,
};
use ipc::{
    IntoWireValue, Listener as _, Protocol as _, RecvError,
    wire::{Cap, MESSAGE_INLINE_MAX},
};
use kernel_objects::{
    Delivery, Error, IssuedHandle, MemoryRegion, Port, ReadOnlyMapping, Resource, Session, kind,
};
use log_abi::{LogBuffer, log};
use managed_component::{Children, Departure, GrantError, Outcome, Purpose, Status};
use runtime::{Mutex, PortTransport, Termination, Timeout};
use syscall::{Handle, ProcessName, Rights};
use userland::{DecodedProgram, ProgramView};
use userland_image::{ExportIndex, IndexedExport, PlanComponent};

use crate::{
    component::{Component, ImporterRequirement, Origin, Record},
    mount::MountThreads,
    relay::LogSources,
};

const LOG_TAG: &str = "rootkeeper";

const BOOT_RECORD: u16 = 0;

const IMAGE_REGION_GRANT: &str = "image region";

const IMPLICIT_MEMORY_LIMIT: u64 = 16 * 1024 * 1024;

const IMPLICIT_RESTARTS: u32 = 0;

const FEATURES_MAX: usize = 256;

/// Срок возврата бюджета перед перезапуском записи.
pub const BUDGET_RETURN_TIMEOUT: Timeout = Timeout::from_secs(5);

type Debt = IssuedHandle<kind::Reply>;

/// Код, с которым завершается процесс rootkeeper. Паника завершает его кодом
/// `runtime::FAILED_EXIT_CODE`, равным коду [`Stopped::Startup`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stopped {
    /// Relay журнала не поднялся либо ресурса композиции не хватило.
    Startup = 1,
    /// Userland-образ, план загрузки либо программа записи недоступны.
    BadImage = 2,
    /// Главный поток вышел из оборота.
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

/// Отказ [`RootSystem::add`].
#[derive(Debug)]
pub enum AddFailure {
    /// Имя записи уже зарегистрировано.
    Name,
    /// Число записей превышает диапазон номера компонента.
    Capacity,
    /// Описание требований, экспортов или бюджета недопустимо либо программа требует признак.
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
    Features(u64),
    Search(u64),
}

struct ImporterBinding {
    name: String,
    protocol: String,
    mode: RequirementMode,
    provider: Option<u16>,
}

#[derive(Clone, Copy)]
struct Export<'a> {
    provider: Provider<'a>,
    protocol: &'a str,
    filter: Attributes<'a>,
}

struct Feature {
    name: String,
    publisher: u64,
}

#[derive(Clone, Copy)]
enum Provider<'a> {
    Record(u16),
    Program {
        name: &'a str,
        executable: &'a str,
        program: DecodedProgram<'a>,
    },
}

struct Search<'a> {
    id: u64,
    owner: u64,
    requirement: String,
    attributes: AttributeSet,
    state: SearchState<'a>,
}

enum SearchState<'a> {
    Enumeration(VecDeque<Export<'a>>),
    Found(Export<'a>),
}

/// Родитель зарегистрированных компонентов. Поток обслуживает записи повтором [`turn`].
pub struct RootSystem<'a> {
    children: Children<'a, Holder, DecodedProgram<'a>>,
    records: Vec<Record<'a>>,
    logs: Vec<Session<log::Protocol>>,
    pub(crate) log_sources: LogSources,
    index: ExportIndex<'a>,
    hardware: Session<hardware::Protocol>,
    image: &'a SystemImage,
    kernel: Handle<kind::Port>,
    restarts_used: Vec<u32>,
    features_closed: Vec<bool>,
    importers: Vec<Vec<ImporterBinding>>,
    searches: Vec<Search<'a>>,
    next_search: u64,
    features: Vec<Feature>,
    resource: Resource,
    pub(crate) mounts: MountThreads,
}

impl Termination for Stopped {
    fn report(self) -> i32 {
        self as i32
    }
}

impl<'a> RootSystem<'a> {
    pub(crate) fn new(
        image: &'a SystemImage,
        kernel: Handle<kind::Port>,
        boot: PlanComponent<'a>,
        manifest: ComponentManifest<'a>,
        index: ExportIndex<'a>,
        log_sources: LogSources,
    ) -> Result<Self, BudgetFailure> {
        let resource = Resource::self_resource().map_err(|_| BudgetFailure::Root)?;
        let children = Children::new().map_err(|_| BudgetFailure::StartupPort)?;
        let hardware = bootstrap::Client::new(PortTransport::new(kernel))
            .acquire_hardware()
            .ok()
            .map(Session::from_session)
            .ok_or(BudgetFailure::Hardware)?;

        Ok(Self {
            children,
            records: alloc::vec![Record {
                name: String::from(boot.name()),
                executable: boot.executable(),
                origin: Origin::Plan,
                manifest,
                restarts: 0,
            }],
            logs: Vec::new(),
            log_sources,
            index,
            hardware,
            image,
            kernel,
            restarts_used: alloc::vec![0],
            features_closed: alloc::vec![false],
            importers: Vec::new(),
            searches: Vec::new(),
            next_search: 0,
            features: Vec::new(),
            resource,
            mounts: MountThreads::default(),
        })
    }

    /// Добавляет запись и возвращает её номер. Программа с признаками отвергается с
    /// [`AddFailure::Declaration`].
    pub fn add(
        &mut self,
        component: Component<'a>,
        log: Session<log::Protocol>,
    ) -> Result<u16, AddFailure> {
        self.insert(component, Origin::Plan, log)
    }

    fn insert(
        &mut self,
        component: Component<'a>,
        origin: Origin,
        log: Session<log::Protocol>,
    ) -> Result<u16, AddFailure> {
        let name = String::from(component.name.as_str());

        if self.record_named(&name).is_some() {
            return Err(AddFailure::Name);
        }
        let manifest =
            decode_manifest(component.program.manifest()).map_err(|_| AddFailure::Declaration)?;

        if component.memory_limit == 0
            || (origin == Origin::Plan && manifest.features().next().is_some())
            || manifest.requirements().any(|requirement| {
                requirement.name().len() > REQUIREMENT_NAME_MAX
                    || requirement.protocol().len() > PROTOCOL_ID_MAX
            })
            || manifest
                .exports()
                .any(|export| export.protocol().len() > PROTOCOL_ID_MAX)
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
            name,
            executable: component.executable,
            origin,
            manifest,
            restarts: component.restarts,
        });
        self.logs.push(log);
        self.restarts_used.push(0);
        self.features_closed.push(false);
        Ok(record)
    }

    /// Создаёт клиентскую capability импортёра с требованиями `requirements`. Родитель отвечает
    /// по ней на `session` и `all` требования из `requirements`. Поток, который зовёт
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
        if !requirements.get(binding).is_some_and(|requirement| {
            declared
                .export(requirement.declaration.protocol())
                .is_some()
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

    fn record_of(&self, executable: &str) -> Option<u16> {
        let record = self
            .records
            .iter()
            .position(|record| record.executable == executable)?;
        u16::try_from(record).ok()
    }

    fn add_implicit(
        &mut self,
        name: &'a str,
        executable: &'a str,
        program: DecodedProgram<'a>,
    ) -> Result<u16, SessionError> {
        let record = u16::try_from(self.records.len()).map_err(|_| SessionError::NoSpace)?;
        let process = ProcessName::new(name).ok_or(SessionError::Refused)?;
        let log = self
            .log_sources
            .register(record, name)
            .ok_or(SessionError::NoSpace)?;
        let component = Component {
            name: process,
            executable,
            program,
            memory_limit: IMPLICIT_MEMORY_LIMIT,
            restarts: IMPLICIT_RESTARTS,
        };
        self.insert(component, Origin::Index, log)
            .map_err(|failure| {
                self.log_sources.retract(record);
                self.log(format_args!("{name} not added: {failure:?}"));
                match failure {
                    AddFailure::Resource(_) | AddFailure::Capacity => SessionError::NoSpace,
                    AddFailure::Name | AddFailure::Declaration => SessionError::Refused,
                }
            })
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
        loop {
            let pending = self.publication_pending();
            let records = &self.records;
            let Some(departure) = self
                .children
                .launch_due(|child| launchable(records, pending, child))
            else {
                return;
            };
            self.decide(departure);
        }
    }

    fn next_launch(&self) -> Timeout {
        let pending = self.publication_pending();
        self.children
            .next_launch(|child| launchable(&self.records, pending, child))
    }

    fn publication_pending(&self) -> bool {
        self.records
            .iter()
            .zip(&self.features_closed)
            .zip(0..)
            .any(|((record, &closed), index)| {
                record.publisher()
                    && !closed
                    && self.children.status(Holder::Record(index)) != Some(Status::Removed)
            })
    }

    fn decide(&mut self, departure: Departure<Holder>) {
        let Holder::Record(record) = departure.child else {
            return;
        };
        if let Some(closed) = self.features_closed.get_mut(usize::from(record)) {
            *closed = false;
        }
        let Some(component) = self.records.get(usize::from(record)) else {
            return;
        };
        let name = component.name.as_str();
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

    fn declared(
        &self,
        holder: Option<Holder>,
        name: &str,
        protocol: &str,
    ) -> Result<(RequirementMode, Option<u16>), SessionError> {
        match holder {
            Some(Holder::Record(record)) => self
                .records
                .get(usize::from(record))
                .and_then(|record| record.requirement(name))
                .filter(|requirement| requirement.protocol() == protocol)
                .map(|requirement| (requirement.mode(), None)),
            Some(Holder::Importer(importer)) => self
                .importers
                .get(importer)
                .and_then(|bindings| {
                    bindings
                        .iter()
                        .find(|binding| binding.name == name && binding.protocol == protocol)
                })
                .map(|binding| (binding.mode, binding.provider)),
            _ => None,
        }
        .ok_or(SessionError::Refused)
    }

    fn matches(
        &self,
        protocol: &str,
        attributes: Attributes<'_>,
        found: &mut dyn FnMut(Export<'a>) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        for (index, record) in self.records.iter().enumerate() {
            let Ok(provider) = u16::try_from(index) else {
                break;
            };

            if let Some(export) = record.export(protocol)
                && self.available(provider)
                && self.published(&record.manifest)
            {
                Self::candidate(Provider::Record(provider), export, attributes, found)?;
            }
        }

        for indexed in self.index.providers(protocol) {
            if let Some((provider, manifest, export)) = self.indexed(indexed, protocol)
                && self.published(&manifest)
            {
                Self::candidate(provider, export, attributes, found)?;
            }
        }

        ControlFlow::Continue(())
    }

    fn candidate(
        provider: Provider<'a>,
        export: ManifestExport<'a>,
        attributes: Attributes<'_>,
        found: &mut dyn FnMut(Export<'a>) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        let Some(filter) = match_filters(export.filters(), attributes) else {
            return ControlFlow::Continue(());
        };

        found(Export {
            provider,
            protocol: export.protocol(),
            filter,
        })
    }

    fn published(&self, manifest: &ComponentManifest<'_>) -> bool {
        manifest.features().all(|required| {
            self.features
                .iter()
                .any(|feature| feature.name.as_str() == required)
        })
    }

    fn indexed(
        &self,
        indexed: IndexedExport<'a>,
        protocol: &str,
    ) -> Option<(Provider<'a>, ComponentManifest<'a>, ManifestExport<'a>)> {
        let name = indexed.name();
        let Some(executable) = indexed.program() else {
            self.log(format_args!("export index entry {name} not decoded"));
            return None;
        };

        if self.record_of(executable).is_some() {
            return None;
        }
        let manifest = self
            .image
            .program(executable)
            .and_then(|program| Some((program, decode_manifest(program.manifest()).ok()?)));
        let Some((program, manifest)) = manifest else {
            self.log(format_args!("export index entry {name} not decoded"));
            return None;
        };
        let export = manifest
            .exports()
            .find(|export| export.protocol() == protocol)?;
        let provider = Provider::Program {
            name,
            executable,
            program,
        };
        Some((provider, manifest, export))
    }

    fn available(&self, provider: u16) -> bool {
        provider == BOOT_RECORD
            || !matches!(
                self.children.status(Holder::Record(provider)),
                None | Some(Status::Removed)
            )
    }

    fn reachable(&self, export: &Export<'a>) -> bool {
        match export.provider {
            Provider::Record(record) => self.available(record),
            Provider::Program { executable, .. } => self
                .record_of(executable)
                .is_none_or(|record| self.available(record)),
        }
    }

    fn only_match(
        &self,
        protocol: &str,
        attributes: Attributes<'_>,
    ) -> Result<Export<'a>, SessionError> {
        only(|found| self.matches(protocol, attributes, found))
    }

    fn pinned(&self, provider: u16, protocol: &str) -> Result<Export<'a>, SessionError> {
        let export = self
            .records
            .get(usize::from(provider))
            .and_then(|record| record.export(protocol))
            .ok_or(SessionError::Refused)?;

        Ok(Export {
            provider: Provider::Record(provider),
            protocol: export.protocol(),
            filter: Attributes::empty(),
        })
    }

    fn open_export(
        &mut self,
        owner: u64,
        requirement: &str,
        export: Export<'a>,
        attributes: Attributes<'_>,
        reply: component_startup::session::Reply<Debt>,
    ) {
        let provider = match export.provider {
            Provider::Record(record) => record,
            Provider::Program {
                name,
                executable,
                program,
            } => match self.record_of(executable) {
                Some(record) => record,
                None => match self.add_implicit(name, executable, program) {
                    Ok(record) => record,
                    Err(error) => {
                        let _ = reply.send(Err::<Port, _>(error));
                        return;
                    }
                },
            },
        };

        if provider == BOOT_RECORD {
            let answer = self.boot_export(owner, requirement, export.protocol);
            let _ = reply.send(answer);
            return;
        }

        if !self.available(provider) {
            let _ = reply.send(Err::<Port, _>(SessionError::Unavailable));
            return;
        }

        self.children.request(
            Holder::Record(provider),
            owner,
            requirement,
            export.protocol,
            AttributeSet::new(attributes),
            reply,
        );
    }

    fn search(&self, holder: Option<Holder>) -> Option<usize> {
        let Some(Holder::Search(id)) = holder else {
            return None;
        };
        // Идентификаторы поисков строго возрастают, в том числе после удаления прежних поисков.
        self.searches
            .binary_search_by_key(&id, |search| search.id)
            .ok()
    }

    fn issue_search(
        &mut self,
        owner: u64,
        requirement: String,
        attributes: AttributeSet,
        state: SearchState<'a>,
    ) -> Result<Port, SessionError> {
        let id = self.next_search;
        self.next_search = id.checked_add(1).ok_or(SessionError::NoSpace)?;
        self.searches
            .try_reserve(1)
            .map_err(|_| SessionError::NoSpace)?;
        let issued =
            self.children
                .issue(owner, &requirement, Purpose::Search, Holder::Search(id))?;
        self.searches.push(Search {
            id,
            owner,
            requirement,
            attributes,
            state,
        });
        Ok(issued)
    }

    fn forget(&mut self, badge: u64, holder: Option<Holder>) {
        if let Some(index) = self.search(holder) {
            self.searches.remove(index);
        } else {
            self.searches.retain(|search| search.owner != badge);
        }

        self.features.retain(|feature| feature.publisher != badge);

        if let Some(Holder::Features(owner)) = holder
            && let Some(Holder::Record(record)) = self.children.caller(owner)
            && let Some(closed) = self.features_closed.get_mut(usize::from(record))
        {
            *closed = true;
        }
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
        let Some(Holder::Record(record)) = self.children.caller(badge) else {
            return Err(SessionError::Refused);
        };

        let session = match protocol {
            hardware::Protocol::ID => self.children.grant(
                badge,
                requirement,
                self.hardware.handle().borrow(),
                Rights::WRITE | Rights::TRANSFER,
            ),
            image::Protocol::ID => self.children.issue(
                badge,
                requirement,
                Purpose::Requirement,
                Holder::Image(badge),
            ),
            features::Protocol::ID if self.planned(record) => self.children.issue(
                badge,
                requirement,
                Purpose::Requirement,
                Holder::Features(badge),
            ),
            _ => return Err(SessionError::Refused),
        };
        session.map_err(SessionError::from)
    }

    fn planned(&self, record: u16) -> bool {
        self.records
            .get(usize::from(record))
            .is_some_and(|record| record.origin == Origin::Plan)
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
        attributes: QueryAttributes<'_>,
        reply: component_startup::session::Reply<Debt>,
    ) {
        let badge = frame.badge;
        let name = requirement.name;
        let protocol = requirement.protocol;
        let Some(attributes) = Attributes::decode(attributes) else {
            let _ = reply.send(Err::<Port, _>(SessionError::Refused));
            return;
        };

        let holder = self.children.caller(badge);
        if let Some(Holder::Record(record)) = holder
            && name == LOG_REQUIREMENT
        {
            let answer = if attributes.is_empty() {
                self.log_session(record, badge, protocol)
            } else {
                Err(SessionError::Refused)
            };
            let _ = reply.send(answer);
            return;
        }

        let export = self
            .declared(holder, name, protocol)
            .and_then(|(_, pinned)| match pinned {
                Some(provider) => self.pinned(provider, protocol),
                None => self.only_match(protocol, attributes),
            });
        match export {
            Ok(export) => self.open_export(badge, name, export, attributes, reply),
            Err(error) => {
                let _ = reply.send(Err::<Port, _>(error));
            }
        }
    }

    fn accept(
        &mut self,
        frame: &Delivery,
        export: &str,
        reply: component_startup::accept::Reply<Debt>,
    ) {
        let provider = match self.children.caller(frame.badge) {
            Some(Holder::Record(record))
                if self
                    .records
                    .get(usize::from(record))
                    .is_some_and(|component| component.export(export).is_some()) =>
            {
                Holder::Record(record)
            }
            _ => {
                let _ = reply.send(Err::<
                    SessionRequest<component_startup::session::Reply<Debt>>,
                    _,
                >(AcceptError::Refused));
                return;
            }
        };
        self.children.accept(provider, frame.badge, export, reply);
    }

    fn all(
        &mut self,
        frame: &Delivery,
        requirement: SessionRequirement<'_>,
        attributes: QueryAttributes<'_>,
    ) -> impl IntoWireValue<Delivery, Read = Result<Cap<kind::Port>, SessionError>> {
        let name = requirement.name;
        let protocol = requirement.protocol;
        let attributes = Attributes::decode(attributes).ok_or(SessionError::Refused)?;
        let (mode, _) = self.declared(self.children.caller(frame.badge), name, protocol)?;
        if mode != RequirementMode::All {
            return Err(SessionError::Refused);
        }

        let mut snapshot = VecDeque::new();
        let listed = self.matches(protocol, attributes, &mut |export| {
            if snapshot.try_reserve(1).is_err() {
                return ControlFlow::Break(());
            }

            snapshot.push_back(export);
            ControlFlow::Continue(())
        });

        if listed.is_break() {
            return Err(SessionError::NoSpace);
        }

        self.issue_search(
            frame.badge,
            String::from(name),
            AttributeSet::new(attributes),
            SearchState::Enumeration(snapshot),
        )
    }

    fn next(&mut self, frame: &Delivery, reply: component_startup::next::Reply<Debt>) {
        let Some(index) = self.search(self.children.caller(frame.badge)) else {
            let _ = reply.send(Err::<Option<Found<Port>>, _>(SessionError::Refused));
            return;
        };

        let export = loop {
            let Some(Search {
                state: SearchState::Enumeration(snapshot),
                ..
            }) = self.searches.get_mut(index)
            else {
                let _ = reply.send(Err::<Option<Found<Port>>, _>(SessionError::Refused));
                return;
            };
            let Some(export) = snapshot.pop_front() else {
                *snapshot = VecDeque::new();
                let _ = reply.send(Ok::<Option<Found<Port>>, SessionError>(None));
                return;
            };
            if self.reachable(&export) {
                break export;
            }
        };

        let Some(search) = self.searches.get(index) else {
            return;
        };
        let (owner, requirement, attributes) = (
            search.owner,
            search.requirement.clone(),
            search.attributes.clone(),
        );
        let found = self
            .issue_search(owner, requirement, attributes, SearchState::Found(export))
            .and_then(|session| {
                Ok(Found {
                    session,
                    protocol: ProtocolName::new(export.protocol).ok_or(SessionError::Refused)?,
                    filter: AttributeSet::new(export.filter),
                })
            });

        let delivered = match found {
            Ok(found) => reply.send(Ok::<_, SessionError>(Some(found))).is_ok(),
            Err(error) => {
                let _ = reply.send(Err::<Option<Found<Port>>, _>(error));
                false
            }
        };

        // Недоставленная выдача закрыта вместе с кадром, и позиция snapshot возвращается.
        if !delivered
            && let Some(Search {
                state: SearchState::Enumeration(snapshot),
                ..
            }) = self.searches.get_mut(index)
        {
            snapshot.push_front(export);
        }
    }

    fn open(&mut self, frame: &Delivery, reply: component_startup::open::Reply<Debt>) {
        let Some(Search {
            owner,
            requirement,
            attributes,
            state: SearchState::Found(export),
            ..
        }) = self
            .search(self.children.caller(frame.badge))
            .and_then(|index| self.searches.get(index))
        else {
            let _ = reply.send(Err::<Port, _>(SessionError::Refused));
            return;
        };

        let (owner, requirement, attributes, export) =
            (*owner, requirement.clone(), attributes.clone(), *export);
        self.open_export(
            owner,
            &requirement,
            export,
            attributes.as_attributes(),
            reply,
        );
    }
}

impl features::Service<Delivery> for RootSystem<'_> {
    fn publish(&mut self, frame: &Delivery, feature: &str) -> Result<(), FeaturesError> {
        let Some(Holder::Features(publisher)) = self.children.caller(frame.badge) else {
            return Err(FeaturesError::Invalid);
        };

        if feature.is_empty() || feature.len() > FEATURE_MAX_BYTES {
            return Err(FeaturesError::Invalid);
        }

        if self
            .features
            .iter()
            .any(|known| known.publisher == publisher && known.name == feature)
        {
            return Ok(());
        }

        if self.features.len() >= FEATURES_MAX || self.features.try_reserve(1).is_err() {
            return Err(FeaturesError::NoSpace);
        }

        self.features.push(Feature {
            name: String::from(feature),
            publisher,
        });
        Ok(())
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
        let next = guard.next_launch();
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
                Some(Holder::Features(_)) => features::dispatch(&mut *guard, incoming),
                _ => component_startup::dispatch(&mut *guard, incoming),
            };
        }
        Err(RecvError::SenderGone(badge)) => {
            let mut guard = system.lock();
            let holder = guard.children.caller(badge);
            if let Some(departure) = guard.children.depart(badge) {
                guard.decide(departure);
            }
            guard.forget(badge, holder);
        }
        Err(RecvError::Timeout | RecvError::Discarded) => {}
        Err(RecvError::Closed) => return ControlFlow::Break(()),
    }
    ControlFlow::Continue(())
}

fn launchable(records: &[Record<'_>], publication_pending: bool, child: Holder) -> bool {
    !publication_pending
        || matches!(child, Holder::Record(record)
            if records.get(usize::from(record)).is_some_and(Record::publisher))
}

fn only<'a>(
    each: impl FnOnce(&mut dyn FnMut(Export<'a>) -> ControlFlow<()>) -> ControlFlow<()>,
) -> Result<Export<'a>, SessionError> {
    let mut only = None;
    let ambiguous = each(&mut |export| match only.replace(export) {
        Some(_) => ControlFlow::Break(()),
        None => ControlFlow::Continue(()),
    });

    if ambiguous.is_break() {
        return Err(SessionError::Refused);
    }

    only.ok_or(SessionError::Unbound)
}

pub(crate) fn log_line(kernel: Handle<kind::Port>, line: Arguments<'_>) {
    let mut message = LogBuffer::<{ log_message_max(LOG_TAG) }>::new();
    let _ = message.write_fmt(line);
    let _ = bootstrap::Client::new(PortTransport::new(kernel)).log_str(LOG_TAG, message.as_str());
}
