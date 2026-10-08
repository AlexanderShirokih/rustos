use alloc::{collections::BTreeMap, string::String, vec::Vec};
use core::{
    any::TypeId,
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
use userland_loader::load_program;

const SESSION_CLOSED_EXIT_CODE: u32 = 0xFFFF_FFFD;

type ImporterDebt = component_startup::session::Reply<IssuedHandle<kind::Reply>>;
type AcceptDebt = component_startup::accept::Reply<IssuedHandle<kind::Reply>>;

/// Записи дочерних компонентов с ключом `C` и отложенные ответы на их запросы `ComponentStartup`.
///
/// [`launch_due`](Self::launch_due) загружает процесс компонента из записи образа `V` с его
/// исполняемым файлом. Один поток обслуживает порт [`port`](Self::port). Уход значения
/// завершает процессы компонентов.
pub struct Children<'a, C, V> {
    port: Port,
    next_badge: AtomicU64,
    entries: Vec<Entry<'a, C, V>>,
    importers: BTreeMap<u64, Importer<C>>,
    holdings: BTreeMap<u64, Holdings>,
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
    /// Копия capability не создана либо вид `K` отличается от прежней выдачи требования.
    Kernel(Error),
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

#[derive(Default)]
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

struct PendingRequest<C, R = ImporterDebt> {
    provider: C,
    protocol: String,
    importer: u64,
    requirement: String,
    attributes: AttributeSet,
    reply: R,
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
    /// Создаёт порт `ComponentStartup` без записей.
    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            port: Port::create()?,
            next_badge: AtomicU64::new(0),
            entries: Vec::new(),
            importers: BTreeMap::new(),
            holdings: BTreeMap::new(),
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
    /// бюджета прежней incarnation. Занятый ключ даёт `SyscallError::InvalidArgument`.
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
            if let Some(outcome) = entry.launch(&self.port, &self.next_badge) {
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
            .or_else(|| self.importers.get(&badge).map(|importer| importer.key))
    }

    /// Ставит заявку импортёра со значком `importer` и атрибутами его запроса `attributes` по
    /// требованию `requirement` на экспорт `protocol` записи `provider`. Заявка сохраняется при
    /// перезапуске. Повтор заявки того же импортёра на тот же экспорт по тому же требованию
    /// заменяет прежнюю и отвечает на неё `SessionError::NoSpace`.
    pub fn request(
        &mut self,
        provider: C,
        importer: u64,
        requirement: &str,
        protocol: &str,
        attributes: AttributeSet,
        reply: ImporterDebt,
    ) {
        let request = PendingRequest {
            provider,
            protocol: String::from(protocol),
            importer,
            requirement: String::from(requirement),
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
    /// в очередь. Повторный запрос того же значка заменяет прежний, и прежний получает
    /// `AcceptError::NoSpace`.
    pub fn accept(&mut self, provider: C, badge: u64, protocol: &str, reply: AcceptDebt) {
        let accept = PendingAccept {
            provider,
            badge,
            protocol: String::from(protocol),
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
        if let Some(importer) = self.importers.remove(&badge) {
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
    /// `requirement`. [`depart`](Self::depart) отзывает выданное. Вид `K`, отличный от прежней
    /// выдачи того же требования, даёт `SyscallError::WrongType`.
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

        let copy = self.holdings.entry(badge).or_default().copy_root::<K>(
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
    /// одной копии не закрывает другие. Уход владельца отзывает все копии его выдачи.
    pub fn issue(
        &mut self,
        owner: u64,
        requirement: &str,
        purpose: Purpose,
        key: C,
    ) -> Result<Port, GrantError> {
        let owner_alive = self.running(owner)
            || self
                .importers
                .get(&owner)
                .is_some_and(|importer| importer.owner.is_none());
        if !owner_alive {
            return Err(GrantError::NotRunning);
        }

        // Запись импортёра существует до минта значка, чтобы обработать событие его ухода.
        let badge = NonZeroU64::MIN.saturating_add(self.next_badge.fetch_add(1, Ordering::Relaxed));
        self.importers.insert(
            badge.get(),
            Importer {
                key,
                owner: Some(owner),
            },
        );

        let rights = Rights::WRITE | Rights::TRANSFER;
        let port = &self.port;
        let holdings = self.holdings.entry(owner).or_default();
        let copy =
            holdings.copy_root::<kind::Port>(requirement, purpose, rights, Some(badge), || {
                port.duplicate(rights | Rights::DUPLICATE, None)
            });
        match copy {
            Ok(copy) => Ok(Port::from(copy.assume_kind::<kind::Port>())),
            Err(error) => {
                self.importers.remove(&badge.get());
                Err(error)
            }
        }
    }

    /// Создаёт клиентскую capability к [`port`](Self::port), запросы которой
    /// [`caller`](Self::caller) называет ключом `key`. Поток, который обслуживает порт, по ней
    /// не вызывает.
    pub fn importer(&mut self, key: C) -> Result<Port, Error> {
        let badge = NonZeroU64::MIN.saturating_add(self.next_badge.fetch_add(1, Ordering::Relaxed));
        let port = self.port.duplicate(Rights::WRITE, Some(badge))?;
        self.importers
            .insert(badge.get(), Importer { key, owner: None });
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
            .retain(|_, importer| importer.owner != Some(owner));
        self.holdings.remove(&owner);
    }
}

impl Holdings {
    fn copy_root<K: ObjectKind + 'static>(
        &mut self,
        requirement: &str,
        purpose: Purpose,
        rights: Rights,
        badge: Option<NonZeroU64>,
        source: impl FnOnce() -> Result<OwnedHandle<K>, Error>,
    ) -> Result<OwnedHandle<kind::Any>, GrantError> {
        let root = self
            .roots
            .iter()
            .find(|grant| grant.requirement == requirement && grant.purpose == purpose);
        match root {
            Some(grant) if grant.kind != TypeId::of::<K>() => {
                Err(GrantError::Kernel(Error::Syscall(SyscallError::WrongType)))
            }
            Some(grant) => Ok(grant.held.borrow().duplicate(rights, badge)?),
            None => {
                let held = source()?.erase();
                let copy = held.borrow().duplicate(rights, badge)?;
                self.roots.push(Grant {
                    requirement: String::from(requirement),
                    purpose,
                    kind: TypeId::of::<K>(),
                    held,
                });
                Ok(copy)
            }
        }
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

    fn launch(&mut self, port: &Port, next_badge: &AtomicU64) -> Option<Outcome> {
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
        match load_program(&self.program, self.name, reserve, session.port) {
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

fn queue_request<C: PartialEq, R>(
    requests: &mut Vec<PendingRequest<C, R>>,
    request: PendingRequest<C, R>,
) -> Option<PendingRequest<C, R>> {
    if let Some(pending) = requests.iter_mut().find(|pending| {
        pending.importer == request.importer
            && pending.requirement == request.requirement
            && pending.provider == request.provider
            && pending.protocol == request.protocol
    }) {
        Some(core::mem::replace(pending, request))
    } else {
        requests.push(request);
        None
    }
}

#[cfg(test)]
mod tests {
    use component_abi::{AttributeSet, Attributes};

    use super::{PendingRequest, String, Vec, queue_request};

    fn request(importer: u64, provider: u16, reply: u16) -> PendingRequest<u16, u16> {
        PendingRequest {
            provider,
            protocol: String::from("Clock"),
            importer,
            requirement: String::from("clocks"),
            attributes: AttributeSet::new(Attributes::empty()),
            reply,
        }
    }

    #[test]
    fn requests_for_different_exports_wait_together_and_only_the_same_export_is_replaced() {
        let mut pending = Vec::new();
        assert!(queue_request(&mut pending, request(10, 1, 1)).is_none());
        assert!(queue_request(&mut pending, request(10, 2, 2)).is_none());
        assert!(queue_request(&mut pending, request(11, 1, 3)).is_none());

        let replaced = queue_request(&mut pending, request(10, 1, 4)).expect("same key replaces");

        assert_eq!(replaced.reply, 1);
        assert_eq!(
            pending
                .iter()
                .map(|request| (request.importer, request.provider, request.reply))
                .collect::<Vec<_>>(),
            [(10, 1, 4), (10, 2, 2), (11, 1, 3)]
        );
    }
}
