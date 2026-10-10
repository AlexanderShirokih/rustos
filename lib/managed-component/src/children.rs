use alloc::{string::String, vec::Vec};
use core::{
    any::TypeId,
    mem::size_of,
    num::NonZeroU64,
    sync::atomic::{AtomicU64, Ordering},
};

use component_abi::{AcceptError, AttributeSet, SessionError, SessionRequest, component_startup};
use kernel_objects::{
    BorrowedHandle, Error, IssuedHandle, ObjectKind, OwnedHandle, Port, Process, Resource, kind,
};
use runtime::{Instant, Timeout};
use syscall::{ProcessName, Rights, SyscallError};
use userland::ProgramView;
use userland_loader::{ImagePages, load_program};

const SESSION_CLOSED_EXIT_CODE: u32 = 0xFFFF_FFFD;

const NO_MEMORY: Error = Error::Syscall(SyscallError::OutOfMemory);

/// Предел ожидающих заявок одного импортёра на один экспорт по одному требованию.
pub const IMPORTER_REQUESTS_PER_EXPORT_MAX: usize = 16;

type ImporterDebt = component_startup::session::Reply<IssuedHandle<kind::Reply>>;
type AcceptDebt = component_startup::accept::Reply<IssuedHandle<kind::Reply>>;

/// Записи дочерних компонентов с ключом `C` и отложенные ответы на их запросы `ComponentStartup`.
///
/// [`launch_due`](Self::launch_due) загружает процесс компонента из записи образа `V` с его
/// исполняемым файлом и отображает сегменты без записи из страниц образа. Один поток обслуживает
/// порт [`port`](Self::port). Уход значения завершает процессы компонентов.
pub struct Children<'a, C, V> {
    pages: ImagePages<'a>,
    port: Port,
    next_badge: AtomicU64,
    entries: Vec<Entry<'a, C, V>>,
    importers: Vec<(u64, Importer<C>)>,
    holdings: Vec<(u64, Holdings)>,
    requests: Vec<PendingRequest<C>>,
    accepts: Vec<PendingAccept<C>>,
}

/// Конец incarnation записи.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Departure<C> {
    pub child: C,
    pub outcome: Outcome,
}

/// Исход incarnation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Процесс завершился с этим кодом выхода.
    Exit(u32),
    /// Компонент закрыл стартовую сессию при живом процессе.
    TakenDown,
    /// Ядро либо загрузчик отказали в запуске процесса.
    LaunchFailed,
    /// Бюджет прежней incarnation не вернулся в срок `budget_deadline`.
    BudgetNotReturned,
}

/// Состояние записи.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Запись ждёт запуска из [`Children::launch_due`].
    Waiting,
    /// Процесс incarnation работает.
    Running,
    /// Incarnation завершилась, и запись ждёт [`Children::restart`] либо [`Children::remove`].
    Departed,
    /// [`Children::remove`] снял запись навсегда.
    Removed,
}

/// Назначение выдачи под требование. Выдачи одного требования с разным назначением имеют
/// разные корни.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// Ресурс, выданный по самому требованию.
    Requirement,
    /// Сессия перечисления экспортов либо результата поиска.
    Search,
}

/// Отказ [`Children::grant`] и [`Children::issue`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantError {
    /// Значок не принадлежит работающей incarnation либо импортёру.
    NotRunning,
    /// Выдача не создана либо вид `K` отличается от прежней выдачи требования.
    Kernel(Error),
}

/// Записи [`Children`] за владельцем: выданные клиентские capability и ожидающие заявки.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Held {
    /// Число выданных клиентских capability и ожидающих заявок.
    pub states: usize,
    /// Память записей выдач и заявок с их строками.
    pub bytes: usize,
}

struct Entry<'a, C, V> {
    child: C,
    program: V,
    name: ProcessName<'a>,
    reserve: Option<Resource>,
    budget_bytes: u64,
    budget_deadline: Timeout,
    state: State,
}

enum State {
    Waiting { exhausted_since: Option<Instant> },
    Running { badge: u64, process: Process },
    Departed,
    Removed,
}

struct Holdings {
    roots: Vec<Grant>,
}

struct Grant {
    requirement: String,
    purpose: Purpose,
    kind: TypeId,
    held: OwnedHandle<kind::Any>,
}

struct Importer<C> {
    key: C,
    owner: Option<u64>,
}

struct PendingRequest<C> {
    provider: C,
    protocol: String,
    importer: u64,
    requirement: String,
    attributes: AttributeSet,
    reply: ImporterDebt,
}

struct PendingAccept<C> {
    provider: C,
    badge: u64,
    protocol: String,
    reply: AcceptDebt,
}

impl From<Error> for GrantError {
    fn from(error: Error) -> Self {
        Self::Kernel(error)
    }
}

impl From<GrantError> for SessionError {
    fn from(error: GrantError) -> Self {
        match error {
            GrantError::NotRunning => Self::Refused,
            GrantError::Kernel(_) => Self::NoSpace,
        }
    }
}

impl<'a, C: Copy + Eq, V: ProgramView<'a>> Children<'a, C, V> {
    /// Создаёт порт `ComponentStartup` без записей. Запуски берут код программ из `pages`.
    pub fn new(pages: ImagePages<'a>) -> Result<Self, Error> {
        Ok(Self {
            pages,
            port: Port::create()?,
            next_badge: AtomicU64::new(0),
            entries: Vec::new(),
            importers: Vec::new(),
            holdings: Vec::new(),
            requests: Vec::new(),
            accepts: Vec::new(),
        })
    }

    /// Серверная сторона порта `ComponentStartup`.
    #[must_use]
    pub const fn port(&self) -> &Port {
        &self.port
    }

    /// Заводит запись `child` в [`Status::Waiting`] с бюджетом `budget_bytes` из `parent_resource`.
    /// Закрытие `parent_resource` отзывает бюджет записи. `budget_deadline` - срок возврата
    /// бюджета прежней incarnation. Отказы:
    /// - `SyscallError::InvalidArgument` - ключ занят;
    /// - `SyscallError::OutOfMemory` - памяти записи не хватило;
    /// - ошибка ядра при выделении бюджета.
    pub fn add(
        &mut self,
        child: C,
        program: V,
        name: ProcessName<'a>,
        parent_resource: &Resource,
        budget_bytes: u64,
        budget_deadline: Timeout,
    ) -> Result<(), Error> {
        if self.entries.iter().any(|entry| entry.child == child) {
            return Err(Error::Syscall(SyscallError::InvalidArgument));
        }

        self.entries.try_reserve(1).map_err(|_| NO_MEMORY)?;

        let reserve = parent_resource.split(budget_bytes)?;
        drop(reserve.split(budget_bytes)?);

        self.entries.push(Entry {
            child,
            program,
            name,
            reserve: Some(reserve),
            budget_bytes,
            budget_deadline,
            state: State::Waiting {
                exhausted_since: None,
            },
        });
        Ok(())
    }

    /// Запускает записи в [`Status::Waiting`], для которых `launchable` истинно, и возвращает
    /// первый отказ запуска. Остальные записи остаются в [`Status::Waiting`]. Вызывается перед
    /// каждым приёмом кадра на [`port`](Self::port).
    pub fn launch_due(&mut self, launchable: impl Fn(C) -> bool) -> Option<Departure<C>> {
        for entry in self
            .entries
            .iter_mut()
            .filter(|entry| launchable(entry.child))
        {
            if let Some(outcome) = entry.launch(&self.pages, &self.port, &self.next_badge) {
                entry.state = State::Departed;
                return Some(Departure {
                    child: entry.child,
                    outcome,
                });
            }
        }

        None
    }

    /// Предельный срок приёма кадра до следующего вызова [`launch_due`](Self::launch_due) с тем
    /// же `launchable`.
    #[must_use]
    pub fn next_launch(&self, launchable: impl Fn(C) -> bool) -> Timeout {
        self.entries
            .iter()
            .filter(|entry| launchable(entry.child))
            .filter_map(Entry::launch_wait)
            .min_by_key(|wait| wait.raw())
            .unwrap_or(Timeout::INFINITE)
    }

    /// Ключ записи компонента либо импортёра со значком `badge`.
    #[must_use]
    pub fn caller(&self, badge: u64) -> Option<C> {
        self.entries
            .iter()
            .find(|entry| entry.running_badge() == Some(badge))
            .map(|entry| entry.child)
            .or_else(|| self.importer_of(badge).map(|importer| importer.key))
    }

    /// Записи за владельцем со значком `owner`.
    #[must_use]
    pub fn held(&self, owner: u64) -> Held {
        self.held_where(|holder| holder == owner)
    }

    /// Записи за всеми владельцами.
    #[must_use]
    pub fn held_total(&self) -> Held {
        self.held_where(|_| true)
    }

    /// Прирост [`Held::bytes`] владельца от одной выдачи [`issue`](Self::issue).
    pub const ISSUE_BYTES: usize = size_of::<(u64, Importer<C>)>();

    /// Прирост [`Held::bytes`] импортёра от заявки [`request`](Self::request) по требованию
    /// `requirement` на экспорт `protocol`.
    #[must_use]
    pub const fn request_bytes(requirement: &str, protocol: &str) -> usize {
        size_of::<PendingRequest<C>>() + requirement.len() + protocol.len()
    }

    /// Ставит заявку импортёра со значком `importer` и атрибутами его запроса `attributes` по
    /// требованию `requirement` на экспорт `protocol` записи `provider`. Заявки выдаются
    /// провайдеру в порядке поступления и сохраняются при перезапуске. Заявка сверх
    /// [`IMPORTER_REQUESTS_PER_EXPORT_MAX`] вытесняет самую давнюю, и та получает
    /// `SessionError::NoSpace`.
    pub fn request(
        &mut self,
        provider: C,
        importer: u64,
        requirement: &str,
        protocol: &str,
        attributes: AttributeSet,
        reply: ImporterDebt,
    ) {
        let prepared = owned(protocol)
            .zip(owned(requirement))
            .filter(|_| self.requests.try_reserve(1).is_ok());
        let Some((protocol_name, requirement)) = prepared else {
            let _ = reply.send(Err::<IssuedHandle<kind::Port>, _>(SessionError::NoSpace));
            return;
        };

        let request = PendingRequest {
            provider,
            protocol: protocol_name,
            importer,
            requirement,
            attributes,
            reply,
        };
        if let Some(replaced) = queue_request(&mut self.requests, request) {
            let _ = replaced
                .reply
                .send(Err::<IssuedHandle<kind::Port>, _>(SessionError::NoSpace));
        }

        self.match_pair(provider, protocol);
    }

    /// Отвечает провайдеру `provider` первой заявкой на экспорт `protocol` либо ставит его запрос
    /// в очередь.
    pub fn accept(&mut self, provider: C, badge: u64, protocol: &str, reply: AcceptDebt) {
        let prepared = owned(protocol).filter(|_| self.accepts.try_reserve(1).is_ok());
        let Some(protocol_name) = prepared else {
            let _ = reply.send(Err::<SessionRequest<ImporterDebt>, _>(AcceptError::NoSpace));
            return;
        };

        let accept = PendingAccept {
            provider,
            badge,
            protocol: protocol_name,
            reply,
        };
        let pending = self
            .accepts
            .iter_mut()
            .find(|pending| pending.badge == badge && pending.protocol == protocol);
        match pending {
            Some(pending) => {
                let replaced = core::mem::replace(pending, accept);
                let _ = replaced
                    .reply
                    .send(Err::<SessionRequest<ImporterDebt>, _>(AcceptError::NoSpace));
            }
            None => self.accepts.push(accept),
        }

        self.match_pair(provider, protocol);
    }

    /// Снимает incarnation либо клиентскую capability, чья сессия со значком `badge`
    /// закрылась. Уход incarnation либо импортёра из [`importer`](Self::importer) отзывает
    /// выданное им. Живой процесс завершается с исходом [`Outcome::TakenDown`]. Клиентская
    /// capability и чужой значок дают `None`.
    pub fn depart(&mut self, badge: u64) -> Option<Departure<C>> {
        if let Ok(index) = self.importer_index(badge) {
            let (_, importer) = self.importers.remove(index);
            if importer.owner.is_none() {
                self.release(badge);
            }
            return None;
        }

        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.running_badge() == Some(badge))?;
        let State::Running { process, .. } = core::mem::replace(&mut entry.state, State::Departed)
        else {
            return None;
        };
        let departure = Departure {
            child: entry.child,
            outcome: outcome_of(&process),
        };

        self.release(badge);
        drop(process);
        Some(departure)
    }

    /// Переводит запись `child` из [`Status::Departed`] в [`Status::Waiting`]. В других
    /// состояниях вызов ничего не делает.
    pub fn restart(&mut self, child: C) {
        if let Some(entry) = self.entry_mut(child)
            && matches!(entry.state, State::Departed)
        {
            entry.state = State::Waiting {
                exhausted_since: None,
            };
        }
    }

    /// Снимает запись `child` в [`Status::Departed`] навсегда. Заявки к её экспортам получают
    /// `SessionError::Unavailable`. В других состояниях вызов ничего не делает.
    pub fn remove(&mut self, child: C) {
        let Some(entry) = self.entry_mut(child) else {
            return;
        };
        if !matches!(entry.state, State::Departed) {
            return;
        }
        entry.state = State::Removed;
        entry.reserve = None;

        let mut index = 0;
        while let Some(pending) = self.requests.get(index) {
            if pending.provider == child {
                let refused = self.requests.remove(index);
                let _ = refused.reply.send(Err::<IssuedHandle<kind::Port>, _>(
                    SessionError::Unavailable,
                ));
            } else {
                index += 1;
            }
        }
    }

    /// Завершает процесс записи `child` в [`Status::Running`] кодом `code`. Исход приходит из
    /// [`depart`](Self::depart).
    pub fn terminate(&mut self, child: C, code: u32) {
        if let Some(entry) = self.entry_mut(child)
            && let State::Running { process, .. } = &entry.state
        {
            let _ = process.terminate(code);
        }
    }

    /// Выдаёт incarnation со значком `badge` копию `source` с правами `rights` под требование
    /// `requirement`. [`depart`](Self::depart) отзывает выданное. Отказы [`GrantError::Kernel`]:
    /// - `SyscallError::WrongType` - вид `K` отличается от прежней выдачи того же требования;
    /// - `SyscallError::OutOfMemory` - памяти корня выдачи не хватило;
    /// - ошибка ядра при копировании `source`.
    pub fn grant<K: ObjectKind + 'static>(
        &mut self,
        badge: u64,
        requirement: &str,
        source: BorrowedHandle<'_, K>,
        rights: Rights,
    ) -> Result<OwnedHandle<K>, GrantError> {
        if !self.running(badge) {
            return Err(GrantError::NotRunning);
        }

        let copy = copy_root::<K>(
            &mut self.holdings,
            badge,
            requirement,
            Purpose::Requirement,
            rights,
            None,
            || source.duplicate(rights | Rights::DUPLICATE, None),
        )?;
        Ok(copy.assume_kind())
    }

    /// Создаёт для incarnation либо импортёра со значком `owner` клиентскую capability к
    /// [`port`](Self::port) под требование `requirement` с назначением `purpose`. Запросы по ней
    /// [`caller`](Self::caller) называет ключом `key`. Каждая копия получает свой значок, и уход
    /// одной копии не закрывает другие. Уход владельца отзывает все копии его выдачи. Отказ
    /// отменяет подготовленные записи выдачи. Отказы [`GrantError::Kernel`] те же, что у
    /// [`grant`](Self::grant).
    pub fn issue(
        &mut self,
        owner: u64,
        requirement: &str,
        purpose: Purpose,
        key: C,
    ) -> Result<Port, GrantError> {
        let owner_alive = self.running(owner)
            || self
                .importer_of(owner)
                .is_some_and(|importer| importer.owner.is_none());
        if !owner_alive {
            return Err(GrantError::NotRunning);
        }

        self.importers.try_reserve(1).map_err(|_| NO_MEMORY)?;

        // Запись импортёра существует до минта значка, чтобы обработать событие его ухода.
        let badge = NonZeroU64::MIN.saturating_add(self.next_badge.fetch_add(1, Ordering::Relaxed));
        let index = self.insert_importer(
            badge.get(),
            Importer {
                key,
                owner: Some(owner),
            },
        );

        let rights = Rights::WRITE | Rights::TRANSFER;
        let port = &self.port;
        let copy = copy_root::<kind::Port>(
            &mut self.holdings,
            owner,
            requirement,
            purpose,
            rights,
            Some(badge),
            || port.duplicate(rights | Rights::DUPLICATE, None),
        );
        match copy {
            Ok(copy) => Ok(Port::from(copy.assume_kind::<kind::Port>())),
            Err(error) => {
                self.importers.remove(index);
                Err(error)
            }
        }
    }

    /// Создаёт клиентскую capability к [`port`](Self::port), запросы которой
    /// [`caller`](Self::caller) называет ключом `key`. Поток, который обслуживает порт, по ней
    /// не вызывает. Отказы:
    /// - `SyscallError::OutOfMemory` - памяти записи не хватило;
    /// - ошибка ядра при копировании порта.
    pub fn importer(&mut self, key: C) -> Result<Port, Error> {
        self.importers.try_reserve(1).map_err(|_| NO_MEMORY)?;
        let badge = NonZeroU64::MIN.saturating_add(self.next_badge.fetch_add(1, Ordering::Relaxed));
        let port = self.port.duplicate(Rights::WRITE, Some(badge))?;
        self.insert_importer(badge.get(), Importer { key, owner: None });
        Ok(port)
    }

    #[must_use]
    pub fn status(&self, child: C) -> Option<Status> {
        let entry = self.entries.iter().find(|entry| entry.child == child)?;

        Some(match entry.state {
            State::Waiting { .. } => Status::Waiting,
            State::Running { .. } => Status::Running,
            State::Departed => Status::Departed,
            State::Removed => Status::Removed,
        })
    }

    fn running(&self, badge: u64) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.running_badge() == Some(badge))
    }

    fn entry_mut(&mut self, child: C) -> Option<&mut Entry<'a, C, V>> {
        self.entries.iter_mut().find(|entry| entry.child == child)
    }

    fn importer_index(&self, badge: u64) -> Result<usize, usize> {
        self.importers
            .binary_search_by_key(&badge, |(badge, _)| *badge)
    }

    fn importer_of(&self, badge: u64) -> Option<&Importer<C>> {
        let index = self.importer_index(badge).ok()?;
        self.importers.get(index).map(|(_, importer)| importer)
    }

    fn insert_importer(&mut self, badge: u64, importer: Importer<C>) -> usize {
        let index = self.importer_index(badge).unwrap_or_else(|index| index);
        self.importers.insert(index, (badge, importer));
        index
    }

    fn held_where(&self, owned_by: impl Fn(u64) -> bool) -> Held {
        let issued = self
            .importers
            .iter()
            .filter(|(_, importer)| importer.owner.is_some_and(&owned_by))
            .count();
        let requests = self
            .requests
            .iter()
            .filter(|request| owned_by(request.importer));

        let mut held = Held {
            states: issued,
            bytes: issued * Self::ISSUE_BYTES,
        };
        for request in requests {
            held.states += 1;
            held.bytes += size_of::<PendingRequest<C>>()
                + request.protocol.capacity()
                + request.requirement.capacity();
        }
        held
    }

    fn match_pair(&mut self, provider: C, protocol: &str) {
        let Some(accept) = self
            .accepts
            .iter()
            .position(|accept| accept.provider == provider && accept.protocol == protocol)
        else {
            return;
        };
        let Some(request) = self
            .requests
            .iter()
            .position(|request| request.provider == provider && request.protocol == protocol)
        else {
            return;
        };

        let accept = self.accepts.remove(accept);
        let PendingRequest {
            provider,
            protocol,
            importer,
            requirement,
            attributes,
            reply,
        } = self.requests.remove(request);
        let sent = accept
            .reply
            .send(Ok::<_, AcceptError>(SessionRequest { reply, attributes }));

        // Недоставленная заявка возвращается на своё место в очереди.
        if let Err(undelivered) = sent
            && let Ok(Ok(SessionRequest { reply, attributes })) = undelivered.into_value()
        {
            self.requests.insert(
                request,
                PendingRequest {
                    provider,
                    protocol,
                    importer,
                    requirement,
                    attributes,
                    reply,
                },
            );
        }
    }

    fn release(&mut self, owner: u64) {
        self.requests.retain(|request| request.importer != owner);
        self.accepts.retain(|accept| accept.badge != owner);
        self.importers
            .retain(|(_, importer)| importer.owner != Some(owner));
        self.holdings.retain(|(holder, _)| *holder != owner);
    }
}

impl<'a, C, V: ProgramView<'a>> Entry<'a, C, V> {
    const fn running_badge(&self) -> Option<u64> {
        match self.state {
            State::Running { badge, .. } => Some(badge),
            _ => None,
        }
    }

    fn launch_wait(&self) -> Option<Timeout> {
        let State::Waiting { exhausted_since } = &self.state else {
            return None;
        };

        Some(exhausted_since.map_or(Timeout::POLL, |since| {
            let elapsed = Instant::now().since(since);
            Timeout::from_ns(self.budget_deadline.raw().saturating_sub(elapsed.raw()))
        }))
    }

    fn launch(
        &mut self,
        pages: &ImagePages<'_>,
        port: &Port,
        next_badge: &AtomicU64,
    ) -> Option<Outcome> {
        let State::Waiting { exhausted_since } = &mut self.state else {
            return None;
        };
        let reserve = self.reserve.as_ref()?;

        match reserve.split(self.budget_bytes) {
            Ok(probe) => drop(probe),
            Err(Error::Syscall(SyscallError::ResourceExhausted)) => {
                let since = *exhausted_since.get_or_insert_with(Instant::now);
                let elapsed = Instant::now().since(since);
                return (elapsed.raw() >= self.budget_deadline.raw())
                    .then_some(Outcome::BudgetNotReturned);
            }
            Err(_) => return Some(Outcome::LaunchFailed),
        }

        let Ok(session) = ::component_startup::mint_session(port, next_badge) else {
            return Some(Outcome::LaunchFailed);
        };
        match load_program(&self.program, Some(pages), self.name, reserve, session.port) {
            Ok(process) => {
                self.state = State::Running {
                    badge: session.badge.get(),
                    process,
                };
                None
            }
            Err(_) => Some(Outcome::LaunchFailed),
        }
    }
}

fn outcome_of(process: &Process) -> Outcome {
    let exited = process.join(Timeout::POLL).is_ok();
    if !exited {
        let _ = process.terminate(SESSION_CLOSED_EXIT_CODE);
        let _ = process.join(Timeout::POLL);
    }

    match process.exit_code() {
        Ok(SESSION_CLOSED_EXIT_CODE) if !exited => Outcome::TakenDown,
        Ok(code) => Outcome::Exit(code),
        // `exit_code` отказывает, если процесс не завершился и после `terminate`.
        Err(_) => Outcome::TakenDown,
    }
}

fn copy_root<K: ObjectKind + 'static>(
    holdings: &mut Vec<(u64, Holdings)>,
    owner: u64,
    requirement: &str,
    purpose: Purpose,
    rights: Rights,
    badge: Option<NonZeroU64>,
    source: impl FnOnce() -> Result<OwnedHandle<K>, Error>,
) -> Result<OwnedHandle<kind::Any>, GrantError> {
    let slot = holdings.binary_search_by_key(&owner, |(owner, _)| *owner);
    let root = slot
        .ok()
        .and_then(|index| holdings.get(index))
        .and_then(|(_, holdings)| {
            holdings
                .roots
                .iter()
                .find(|grant| grant.requirement == requirement && grant.purpose == purpose)
        });

    match root {
        Some(grant) if grant.kind != TypeId::of::<K>() => {
            return Err(GrantError::Kernel(Error::Syscall(SyscallError::WrongType)));
        }
        Some(grant) => return Ok(grant.held.borrow().duplicate(rights, badge)?),
        None => {}
    }

    let requirement = owned(requirement).ok_or(NO_MEMORY)?;
    let mut fresh = Vec::new();
    let reserved = match slot {
        Ok(index) => holdings
            .get_mut(index)
            .is_some_and(|(_, holdings)| holdings.roots.try_reserve(1).is_ok()),
        Err(_) => holdings.try_reserve(1).is_ok() && fresh.try_reserve_exact(1).is_ok(),
    };
    if !reserved {
        return Err(GrantError::Kernel(NO_MEMORY));
    }

    let held = source()?.erase();
    let copy = held.borrow().duplicate(rights, badge)?;
    let grant = Grant {
        requirement,
        purpose,
        kind: TypeId::of::<K>(),
        held,
    };
    match slot {
        Ok(index) => {
            if let Some((_, holdings)) = holdings.get_mut(index) {
                holdings.roots.push(grant);
            }
        }
        Err(index) => {
            fresh.push(grant);
            holdings.insert(index, (owner, Holdings { roots: fresh }));
        }
    }
    Ok(copy)
}

fn owned(text: &str) -> Option<String> {
    let mut owned = String::new();
    owned.try_reserve_exact(text.len()).ok()?;
    owned.push_str(text);
    Some(owned)
}

fn queue_request<C: PartialEq>(
    requests: &mut Vec<PendingRequest<C>>,
    request: PendingRequest<C>,
) -> Option<PendingRequest<C>> {
    let same_key = |pending: &PendingRequest<C>| {
        pending.importer == request.importer
            && pending.requirement == request.requirement
            && pending.provider == request.provider
            && pending.protocol == request.protocol
    };
    let waiting = requests.iter().filter(|pending| same_key(pending)).count();
    let oldest = requests.iter().position(same_key);

    let evicted = match oldest {
        Some(oldest) if waiting >= IMPORTER_REQUESTS_PER_EXPORT_MAX => {
            Some(requests.remove(oldest))
        }
        _ => None,
    };
    requests.push(request);
    evicted
}
