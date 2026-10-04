//! Записи компонентов и обслуживание их требований родителем.

use alloc::{collections::VecDeque, string::String, vec::Vec};
use core::{
    fmt::{Arguments, Write as _},
    ops::ControlFlow,
};

use bootstrap_abi::{ImageError, LOG_MESSAGE_MAX, bootstrap, hardware, image};
use component_abi::{
    AcceptError, AttributeSet, Attributes, ComponentManifest, Found, LOG_REQUIREMENT,
    PROTOCOL_ID_MAX, ProtocolName, QueryAttributes, REQUIREMENT_NAME_MAX, RequirementDeclaration,
    RequirementMode, SessionError, SessionRequest, SessionRequirement, component_startup,
    decode_manifest, match_filters,
};
use ipc::{
    IntoWireValue, Listener as _, Protocol as _, RecvError,
    wire::{Cap, MESSAGE_INLINE_MAX, Str},
};
use kernel_objects::{
    Delivery, Error, IssuedHandle, MemoryRegion, Port, ReadOnlyMapping, Resource, Session, kind,
};
use log_abi::{LogBuffer, log};
use managed_component::{Children, Departure, GrantError, Outcome, Purpose, Status};
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
    provider: u16,
    protocol: &'a str,
    filter: Attributes<'a>,
}

struct Search<'a> {
    id: u64,
    owner: u64,
    requirement: String,
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
    hardware: Session<hardware::Protocol>,
    image: &'a SystemImage,
    kernel: Handle<kind::Port>,
    restarts_used: Vec<u32>,
    importers: Vec<Vec<ImporterBinding>>,
    searches: Vec<Search<'a>>,
    next_search: u64,
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
            searches: Vec::new(),
            next_search: 0,
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
            name: component.name.as_str(),
            manifest,
            restarts: component.restarts,
        });
        self.logs.push(log);
        self.restarts_used.push(0);
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

    fn matches<'s>(
        &'s self,
        protocol: &'s str,
        attributes: Attributes<'s>,
    ) -> impl Iterator<Item = Export<'a>> + 's {
        self.records
            .iter()
            .enumerate()
            .filter_map(move |(index, record)| {
                let provider = u16::try_from(index).ok()?;
                let export = record.export(protocol)?;
                let filter = match_filters(export.filters(), attributes)?;
                self.available(provider).then_some(Export {
                    provider,
                    protocol: export.protocol(),
                    filter,
                })
            })
    }

    fn available(&self, provider: u16) -> bool {
        provider == BOOT_RECORD
            || !matches!(
                self.children.status(Holder::Record(provider)),
                None | Some(Status::Removed)
            )
    }

    fn only_match(
        &self,
        protocol: &str,
        attributes: Attributes<'_>,
    ) -> Result<Export<'a>, SessionError> {
        let mut matches = self.matches(protocol, attributes);
        let export = matches.next().ok_or(SessionError::Unbound)?;
        if matches.next().is_some() {
            return Err(SessionError::Refused);
        }
        Ok(export)
    }

    fn pinned(&self, provider: u16, protocol: &str) -> Result<Export<'a>, SessionError> {
        self.records
            .get(usize::from(provider))
            .and_then(|record| record.export(protocol))
            .map(|export| Export {
                provider,
                protocol: export.protocol(),
                filter: Attributes::empty(),
            })
            .ok_or(SessionError::Refused)
    }

    fn open_export(
        &mut self,
        owner: u64,
        requirement: &str,
        export: Export<'a>,
        reply: component_startup::session::Reply<Debt>,
    ) {
        if export.provider == BOOT_RECORD {
            let answer = self.boot_export(owner, requirement, export.protocol);
            let _ = reply.send(answer);
            return;
        }

        if !self.available(export.provider) {
            let _ = reply.send(Err::<Port, _>(SessionError::Unavailable));
            return;
        }

        self.children.request(
            Holder::Record(export.provider),
            owner,
            requirement,
            export.protocol,
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
        if !matches!(self.children.caller(badge), Some(Holder::Record(_))) {
            return Err(SessionError::Refused);
        }

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
        attributes: QueryAttributes<'_>,
        reply: component_startup::session::Reply<Debt>,
    ) {
        let badge = frame.badge;
        let name = requirement.name.as_str();
        let protocol = requirement.protocol.as_str();
        let Some(attributes) = Attributes::decode(attributes.as_bytes()) else {
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
            Ok(export) => self.open_export(badge, name, export, reply),
            Err(error) => {
                let _ = reply.send(Err::<Port, _>(error));
            }
        }
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
        let name = requirement.name.as_str();
        let protocol = requirement.protocol.as_str();
        let attributes = Attributes::decode(attributes.as_bytes()).ok_or(SessionError::Refused)?;
        let (mode, _) = self.declared(self.children.caller(frame.badge), name, protocol)?;
        if mode != RequirementMode::All {
            return Err(SessionError::Refused);
        }

        let mut snapshot = VecDeque::new();
        for export in self.matches(protocol, attributes) {
            snapshot.try_reserve(1).map_err(|_| SessionError::NoSpace)?;
            snapshot.push_back(export);
        }

        self.issue_search(
            frame.badge,
            String::from(name),
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
            if self.available(export.provider) {
                break export;
            }
        };

        let Some(search) = self.searches.get(index) else {
            return;
        };
        let (owner, requirement) = (search.owner, search.requirement.clone());
        let found = self
            .issue_search(owner, requirement, SearchState::Found(export))
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
            state: SearchState::Found(export),
            ..
        }) = self
            .search(self.children.caller(frame.badge))
            .and_then(|index| self.searches.get(index))
        else {
            let _ = reply.send(Err::<Port, _>(SessionError::Refused));
            return;
        };

        let (owner, requirement, export) = (*owner, requirement.clone(), *export);
        self.open_export(owner, &requirement, export, reply);
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

pub(crate) fn log_line(kernel: Handle<kind::Port>, line: Arguments<'_>) {
    let mut message = LogBuffer::<LOG_MESSAGE_MAX>::new();
    let _ = message.write_fmt(line);
    let _ = bootstrap::Client::new(PortTransport::new(kernel)).log_str(LOG_TAG, message.as_str());
}
