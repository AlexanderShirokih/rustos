//! Тестовый родитель поверх `Children`: записи фикстур, ответы на их требования и оборот
//! цикла родителя в своём потоке.

use alloc::{sync::Arc, vec::Vec};
use core::{num::NonZeroU64, ops::ControlFlow};

use component_abi::{
    AcceptError, LOG_REQUIREMENT, PROTOCOL_ID_MAX, REQUIREMENT_NAME_MAX, ResourceError,
    SessionError, SessionRequest, SessionRequirement, SessionScope,
    component_startup as startup_contract,
};
use ipc::{
    Listener as _, Protocol as _, RecvError,
    wire::{MESSAGE_INLINE_MAX, Str},
};
use kernel_objects::{Delivery, Error, IssuedHandle, OwnedHandle, Port, Resource, Session, kind};
use log_abi::log;
use managed_component::{Children, Departure, GrantError, Status};
use runtime::{Instant, Mutex, PortTransport, Timeout};
use syscall::{Handle, Rights};
use userland_image::DecodedProgram;

use crate::{
    composition::{TurnThread, pause},
    fixture::{process_bytes, program},
    log_relay::component_log_session,
};

type Debt = IssuedHandle<kind::Reply>;

const CHILD_SLACK: u64 = 1024 * 1024;

const CHILD_LOG_BADGE: NonZeroU64 = NonZeroU64::new(0x0500_0000).expect("badge is non-zero");

const BUDGET_DEADLINE: Timeout = Timeout::from_secs(1);

const TURN: Timeout = Timeout::from_millis(20);
const POLL: Timeout = Timeout::from_millis(10);
const REQUEST_WAIT: Timeout = Timeout::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    Record(u16),
    Importer(u16),
}

pub(crate) enum Answer {
    Export {
        provider: u16,
        protocol: &'static str,
        scope: &'static str,
    },
    Grant {
        protocol: &'static str,
        source: Port,
    },
    Transfer {
        protocol: &'static str,
        source: Option<Port>,
    },
    Refuse,
}

pub(crate) struct Child {
    program: DecodedProgram<'static>,
    name: &'static str,
    budget: u64,
    requirements: Vec<(&'static str, Answer)>,
    exports: Vec<&'static str>,
}

pub(crate) struct Parent {
    _turn: TurnThread,
    state: Arc<Mutex<State>>,
    resource: Resource,
}

struct State {
    children: Children<'static, Key, DecodedProgram<'static>>,
    records: Vec<Record>,
    importers: Vec<Vec<(&'static str, Answer)>>,
    departures: Vec<Departure<u16>>,
    accepts: usize,
    requests: usize,
    last_depart: Timeout,
}

struct Record {
    requirements: Vec<(&'static str, Answer)>,
    exports: Vec<&'static str>,
    badge: Option<u64>,
}

impl Answer {
    pub(crate) fn log(source: Session<log::Protocol>) -> Self {
        Self::Grant {
            protocol: log::Protocol::ID,
            source: source.into_handle(),
        }
    }

    pub(crate) const fn export<P: ipc::Protocol>(provider: u16) -> Self {
        Self::Export {
            provider,
            protocol: P::ID,
            scope: "",
        }
    }
}

impl Child {
    pub(crate) fn new(path: &str, name: &'static str) -> Self {
        let program = program(path);
        let log = component_log_session(CHILD_LOG_BADGE).map_or(Answer::Refuse, Answer::log);
        Self {
            budget: process_bytes(&program) + CHILD_SLACK,
            program,
            name,
            requirements: alloc::vec![(LOG_REQUIREMENT, log)],
            exports: Vec::new(),
        }
    }

    pub(crate) fn log(mut self, answer: Answer) -> Self {
        self.requirements
            .retain(|(requirement, _)| *requirement != LOG_REQUIREMENT);
        self.requirements.push((LOG_REQUIREMENT, answer));
        self
    }

    pub(crate) fn requirement(mut self, requirement: &'static str, answer: Answer) -> Self {
        self.requirements.push((requirement, answer));
        self
    }

    pub(crate) fn export<P: ipc::Protocol>(mut self) -> Self {
        self.exports.push(P::ID);
        self
    }

    pub(crate) const fn budget(mut self, bytes: u64) -> Self {
        self.budget = bytes;
        self
    }
}

impl Parent {
    pub(crate) fn start() -> Self {
        let children = Children::new().expect("startup port created");
        let port = children.port().as_raw();
        let state = Arc::new(Mutex::new(State {
            children,
            records: Vec::new(),
            importers: Vec::new(),
            departures: Vec::new(),
            accepts: 0,
            requests: 0,
            last_depart: Timeout::POLL,
        }));
        let turned = Arc::clone(&state);
        Self {
            _turn: TurnThread::start(move || turn(&turned, port)),
            state,
            resource: Resource::self_resource().expect("self metering resource"),
        }
    }

    pub(crate) fn add(&self, child: Child) -> u16 {
        self.add_on(child, &self.resource)
            .expect("child record added")
    }

    pub(crate) fn add_on(&self, child: Child, resource: &Resource) -> Result<u16, Error> {
        let mut state = self.state.lock();
        let key = u16::try_from(state.records.len()).expect("record keys fit u16");
        state.children.add(
            Key::Record(key),
            child.program,
            child.name,
            resource,
            child.budget,
            BUDGET_DEADLINE,
        )?;
        state.records.push(Record {
            requirements: child.requirements,
            exports: child.exports,
            badge: None,
        });
        Ok(key)
    }

    pub(crate) fn importer(&self, requirements: Vec<(&'static str, Answer)>) -> Port {
        let mut state = self.state.lock();
        let key = u16::try_from(state.importers.len()).expect("importer keys fit u16");
        let port = state
            .children
            .importer(Key::Importer(key))
            .expect("importer capability created");
        state.importers.push(requirements);
        port
    }

    pub(crate) fn departure(&self, wait: Timeout) -> Option<Departure<u16>> {
        self.poll(wait, |state| {
            (!state.departures.is_empty()).then(|| state.departures.remove(0))
        })
    }

    pub(crate) fn await_requests(&self, count: usize) -> bool {
        self.poll(REQUEST_WAIT, |state| {
            (state.requests >= count).then_some(())
        })
        .is_some()
    }

    pub(crate) fn await_accepts(&self, count: usize) -> bool {
        self.poll(REQUEST_WAIT, |state| (state.accepts >= count).then_some(()))
            .is_some()
    }

    pub(crate) fn restart(&self, child: u16) {
        self.state.lock().children.restart(Key::Record(child));
    }

    pub(crate) fn terminate(&self, child: u16, code: u32) {
        self.state
            .lock()
            .children
            .terminate(Key::Record(child), code);
    }

    pub(crate) fn status(&self, child: u16) -> Option<Status> {
        self.state.lock().children.status(Key::Record(child))
    }

    pub(crate) fn accepts(&self) -> usize {
        self.state.lock().accepts
    }

    pub(crate) fn requests(&self) -> usize {
        self.state.lock().requests
    }

    pub(crate) fn badge(&self, child: u16) -> Option<u64> {
        self.state
            .lock()
            .records
            .get(usize::from(child))
            .and_then(|record| record.badge)
    }

    pub(crate) fn last_depart(&self) -> Timeout {
        self.state.lock().last_depart
    }

    pub(crate) fn log_copy(&self, child: u16) -> Result<Port, GrantError> {
        let mut guard = self.state.lock();
        let state = &mut *guard;
        let record = state
            .records
            .get(usize::from(child))
            .ok_or(GrantError::NotRunning)?;
        let badge = record.badge.ok_or(GrantError::NotRunning)?;
        let Some((_, Answer::Grant { source, .. })) = record
            .requirements
            .iter()
            .find(|(requirement, _)| *requirement == LOG_REQUIREMENT)
        else {
            return Err(GrantError::NotRunning);
        };
        state.children.grant(
            badge,
            LOG_REQUIREMENT,
            source.borrow(),
            Rights::WRITE | Rights::TRANSFER,
        )
    }

    pub(crate) fn log_copy_as_resource(&self, child: u16) -> Result<Resource, GrantError> {
        let mut guard = self.state.lock();
        let state = &mut *guard;
        let record = state
            .records
            .get(usize::from(child))
            .ok_or(GrantError::NotRunning)?;
        let badge = record.badge.ok_or(GrantError::NotRunning)?;
        state
            .children
            .grant(badge, LOG_REQUIREMENT, self.resource.borrow(), Rights::READ)
    }

    pub(crate) fn outsider(&self, badge: NonZeroU64) -> Port {
        self.state
            .lock()
            .children
            .port()
            .duplicate(Rights::WRITE, Some(badge))
            .expect("outsider copy of the startup port")
    }

    fn poll<T>(&self, wait: Timeout, mut check: impl FnMut(&mut State) -> Option<T>) -> Option<T> {
        let since = Instant::now();
        loop {
            if let Some(found) = check(&mut self.state.lock()) {
                return Some(found);
            }
            if Instant::now().since(since).raw() >= wait.raw() {
                return None;
            }
            pause(POLL);
        }
    }
}

impl State {
    fn depart(&mut self, departure: Departure<Key>) {
        if let Key::Record(child) = departure.child {
            self.departures.push(Departure {
                child,
                outcome: departure.outcome,
            });
        }
    }

    fn caller(&mut self, badge: u64) -> Option<Key> {
        let caller = self.children.caller(badge);
        if let Some(Key::Record(child)) = caller
            && let Some(record) = self.records.get_mut(usize::from(child))
        {
            record.badge = Some(badge);
        }
        caller
    }

    fn refuse_resource<R>(&mut self, badge: u64) -> Result<R, ResourceError> {
        self.caller(badge);
        Err(ResourceError::Refused)
    }
}

impl startup_contract::Service<Delivery> for State {
    fn session(
        &mut self,
        frame: &Delivery,
        requirement: SessionRequirement<'_>,
        reply: startup_contract::session::Reply<Debt>,
    ) {
        let badge = frame.badge;
        let name = requirement.name.as_str();
        let protocol = requirement.protocol.as_str();
        let requirements = match self.caller(badge) {
            Some(Key::Record(child)) => self
                .records
                .get_mut(usize::from(child))
                .map_or(&mut [][..], |record| record.requirements.as_mut_slice()),
            Some(Key::Importer(importer)) => self
                .importers
                .get_mut(usize::from(importer))
                .map_or(&mut [][..], Vec::as_mut_slice),
            None => &mut [],
        };
        let answer = requirements
            .iter_mut()
            .find(|(bound, _)| *bound == name)
            .map(|(_, answer)| answer);

        match answer {
            Some(Answer::Export {
                provider,
                protocol: exported,
                scope,
            }) if *exported == protocol => {
                let provider = Key::Record(*provider);
                let scope = SessionScope::new(scope);
                match (self.children.status(provider), scope) {
                    (None | Some(Status::Removed), _) => {
                        let _ = reply.send(Err::<Port, _>(SessionError::Unavailable));
                    }
                    (_, None) => {
                        let _ = reply.send(Err::<Port, _>(SessionError::Refused));
                    }
                    (_, Some(scope)) => {
                        self.requests += 1;
                        self.children
                            .request(provider, exported, badge, name, scope, reply);
                    }
                }
            }
            Some(Answer::Grant {
                protocol: granted,
                source,
            }) if *granted == protocol => {
                let answer = self
                    .children
                    .grant(
                        badge,
                        name,
                        source.borrow(),
                        Rights::WRITE | Rights::TRANSFER,
                    )
                    .map_err(SessionError::from);
                let _ = reply.send(answer);
            }
            Some(Answer::Transfer {
                protocol: transferred,
                source,
            }) if *transferred == protocol => {
                let _ = reply.send(source.take().ok_or(SessionError::Refused));
            }
            _ => {
                let _ = reply.send(Err::<Port, _>(SessionError::Refused));
            }
        }
    }

    fn region(
        &mut self,
        frame: &Delivery,
        _requirement: Str<'_, { REQUIREMENT_NAME_MAX }>,
        reply: startup_contract::region::Reply<Debt>,
    ) {
        let _ = reply.send(self.refuse_resource::<OwnedHandle<kind::MemoryRegion>>(frame.badge));
    }

    fn accept(
        &mut self,
        frame: &Delivery,
        export: Str<'_, { PROTOCOL_ID_MAX }>,
        reply: startup_contract::accept::Reply<Debt>,
    ) {
        self.accepts += 1;
        let export = export.as_str();
        let provider = match self.caller(frame.badge) {
            Some(Key::Record(child))
                if self
                    .records
                    .get(usize::from(child))
                    .is_some_and(|record| record.exports.contains(&export)) =>
            {
                Key::Record(child)
            }
            Some(Key::Record(_) | Key::Importer(_)) | None => {
                let _ = reply.send(Err::<
                    SessionRequest<startup_contract::session::Reply<Debt>>,
                    _,
                >(AcceptError::Refused));
                return;
            }
        };
        self.children.accept(provider, frame.badge, export, reply);
    }
}

fn turn(state: &Mutex<State>, port: Handle<kind::Port>) -> ControlFlow<()> {
    let wait = {
        let mut state = state.lock();
        while let Some(departure) = state.children.launch_due() {
            state.depart(departure);
        }
        let next = state.children.next_launch();
        if next.raw() < TURN.raw() { next } else { TURN }
    };

    let mut bytes = [0u8; MESSAGE_INLINE_MAX];
    match PortTransport::new(port).recv(&mut bytes, wait) {
        Ok(incoming) => {
            let _ = startup_contract::dispatch(&mut *state.lock(), incoming);
        }
        Err(RecvError::SenderGone(badge)) => {
            let mut state = state.lock();
            let since = Instant::now();
            if let Some(departure) = state.children.depart(badge) {
                state.last_depart = Instant::now().since(since);
                state.depart(departure);
            }
        }
        Err(RecvError::Timeout | RecvError::Discarded) => {}
        Err(RecvError::Closed) => return ControlFlow::Break(()),
    }
    ControlFlow::Continue(())
}
