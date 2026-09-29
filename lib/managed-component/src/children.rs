use alloc::{string::String, vec::Vec};
use core::{
    any::TypeId,
    num::NonZeroU64,
    sync::atomic::{AtomicU64, Ordering},
};

use component_abi::{
    AcceptError, ResourceError, SessionError, SessionRequest, SessionScope, component_startup,
};
use kernel_objects::{
    BorrowedHandle, Error, IssuedHandle, ObjectKind, OwnedHandle, Port, Process, Resource, kind,
};
use runtime::{Instant, Timeout};
use syscall::{Rights, SyscallError};
use userland::EntryView;
use userland_loader::load_entry;

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
    importers: Vec<Importer<C>>,
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

/// Отказ [`Children::grant`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantError {
    /// Значок не принадлежит работающей incarnation.
    NotRunning,
    /// Копия capability не создана либо вид `K` отличается от прежней выдачи требования.
    Kernel(Error),
}

struct Entry<'a, C, V> {
    child: C,
    program: V,
    name: &'a str,
    reserve: Option<Resource>,
    budget_bytes: u64,
    budget_deadline: Timeout,
    state: State,
}

enum State {
    Waiting {
        exhausted_since: Option<Instant>,
    },
    Running {
        badge: u64,
        process: Process,
        grants: Vec<Grant>,
    },
    Departed,
    Removed,
}

struct Grant {
    requirement: String,
    kind: TypeId,
    held: OwnedHandle<kind::Any>,
}

struct Importer<C> {
    key: C,
    badge: u64,
}

struct PendingRequest<C> {
    provider: C,
    protocol: String,
    importer: u64,
    requirement: String,
    scope: SessionScope,
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

impl From<GrantError> for ResourceError {
    fn from(error: GrantError) -> Self {
        match error {
            GrantError::NotRunning => Self::Refused,
            GrantError::Kernel(_) => Self::NoSpace,
        }
    }
}

impl<'a, C: Copy + Eq, V: EntryView<'a>> Children<'a, C, V> {
    /// Создаёт порт `ComponentStartup` без записей.
    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            port: Port::create()?,
            next_badge: AtomicU64::new(0),
            entries: Vec::new(),
            importers: Vec::new(),
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
        name: &'a str,
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

    /// Запускает записи в [`Status::Waiting`] и возвращает первый отказ запуска. Вызывается перед
    /// каждым приёмом кадра на [`port`](Self::port).
    pub fn launch_due(&mut self) -> Option<Departure<C>> {
        for entry in &mut self.entries {
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

    /// Предельный срок приёма кадра до следующего вызова [`launch_due`](Self::launch_due).
    #[must_use]
    pub fn next_launch(&self) -> Timeout {
        self.entries
            .iter()
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
            .or_else(|| {
                self.importers
                    .iter()
                    .find(|importer| importer.badge == badge)
                    .map(|importer| importer.key)
            })
    }

    /// Ставит в очередь заявку импортёра `importer` на экспорт `protocol` записи `provider`.
    /// Заявка остаётся в очереди при перезапуске провайдера. Повторная заявка на то же
    /// требование заменяет прежнюю, и прежняя получает `SessionError::NoSpace`.
    pub fn request(
        &mut self,
        provider: C,
        protocol: &str,
        importer: u64,
        requirement: &str,
        scope: SessionScope,
        reply: ImporterDebt,
    ) {
        let request = PendingRequest {
            provider,
            protocol: String::from(protocol),
            importer,
            requirement: String::from(requirement),
            scope,
            reply,
        };
        let pending = self
            .requests
            .iter_mut()
            .find(|pending| pending.importer == importer && pending.requirement == requirement);
        match pending {
            Some(pending) => {
                let replaced = core::mem::replace(pending, request);
                let _ = replaced
                    .reply
                    .send(Err::<IssuedHandle<kind::Port>, _>(SessionError::NoSpace));
            }
            None => self.requests.push(request),
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

    /// Снимает incarnation либо импортёра, чья сессия со значком `badge` закрылась, и отзывает
    /// выданное им. Живой процесс завершается с исходом [`Outcome::TakenDown`]. Импортёр и
    /// чужой значок дают `None`.
    pub fn depart(&mut self, badge: u64) -> Option<Departure<C>> {
        if let Some(index) = self
            .importers
            .iter()
            .position(|importer| importer.badge == badge)
        {
            self.importers.swap_remove(index);
            self.drop_debts(badge);
            return None;
        }

        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.running_badge() == Some(badge))?;
        let State::Running {
            process, grants, ..
        } = core::mem::replace(&mut entry.state, State::Departed)
        else {
            return None;
        };
        let departure = Departure {
            child: entry.child,
            outcome: outcome_of(&process),
        };
        drop(grants);
        drop(process);

        self.drop_debts(badge);
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
        let grants = self
            .entries
            .iter_mut()
            .find_map(|entry| match &mut entry.state {
                State::Running {
                    badge: running,
                    grants,
                    ..
                } if *running == badge => Some(grants),
                _ => None,
            })
            .ok_or(GrantError::NotRunning)?;

        let kind = TypeId::of::<K>();
        if let Some(grant) = grants.iter().find(|grant| grant.requirement == requirement) {
            if grant.kind != kind {
                return Err(GrantError::Kernel(Error::Syscall(SyscallError::WrongType)));
            }
            let copy = grant.held.duplicate(rights, None)?;
            return Ok(copy.assume_kind());
        }

        let held = source.duplicate(rights | Rights::DUPLICATE, None)?;
        let copy = held.duplicate(rights, None)?;
        grants.push(Grant {
            requirement: String::from(requirement),
            kind,
            held: held.erase(),
        });
        Ok(copy)
    }

    /// Создаёт клиентскую capability к [`port`](Self::port), запросы которой
    /// [`caller`](Self::caller) называет ключом `key`. Поток, который обслуживает порт, по ней
    /// не вызывает.
    pub fn importer(&mut self, key: C) -> Result<Port, Error> {
        let badge = NonZeroU64::MIN.saturating_add(self.next_badge.fetch_add(1, Ordering::Relaxed));
        let port = self.port.duplicate(Rights::WRITE, Some(badge))?;
        self.importers.push(Importer {
            key,
            badge: badge.get(),
        });
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
            scope,
            reply,
        } = self.requests.remove(request);
        let sent = accept
            .reply
            .send(Ok::<_, AcceptError>(SessionRequest { scope, reply }));

        // Недоставленная заявка возвращается на своё место в очереди.
        if let Err(undelivered) = sent
            && let Ok(Ok(SessionRequest { scope, reply })) = undelivered.into_value()
        {
            self.requests.insert(
                request,
                PendingRequest {
                    provider,
                    protocol,
                    importer,
                    requirement,
                    scope,
                    reply,
                },
            );
        }
    }

    fn drop_debts(&mut self, badge: u64) {
        self.requests.retain(|request| request.importer != badge);
        self.accepts.retain(|accept| accept.badge != badge);
    }
}

impl<'a, C, V: EntryView<'a>> Entry<'a, C, V> {
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
        match load_entry(&self.program, self.name, reserve, session.port) {
            Ok(process) => {
                self.state = State::Running {
                    badge: session.badge.get(),
                    process,
                    grants: Vec::new(),
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
