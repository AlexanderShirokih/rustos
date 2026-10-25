#![no_std]
#![no_main]

use core::num::NonZeroU64;

use component_startup::{
    AsyncStartup, Requirements as _, SessionPort, SessionRequest, accept, component_manifest,
};
use executor::{Executor, PortChannel, Reactor};
use ipc::CallError;
use kernel_objects::Port;
use runtime::{FAILED_EXIT_CODE, Timeout, exit, log_line, process_exit, spawn_bound};
use syscall::{Handle, Rights, kind};

mod child_steps_protocol;
mod failed;
mod probe_protocol;

use child_steps_protocol::{
    Report, STEP_LINE, STEP_PANIC, Step, accept_error_code, action, child_steps, ipc_error_code,
    outcome, session_error_of,
};
use failed::Failed;
use probe_protocol::{Echo, probe};

component_manifest! {
    struct StepsStartup {
        requirements: { steps: session(child_steps::Protocol) },
        exports: [probe::Protocol],
    }
}

struct Fixture {
    parent: Handle<kind::Port>,
    export: SessionPort,
    held: Option<SessionRequest>,
    reactor: Reactor,
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Failed> {
    let executor = Executor::new().map_err(|_| Failed)?;
    let reactor = executor.reactor();

    executor.block_on(async {
        let steps = StepsStartup::from_parent(parent)
            .steps
            .open_async(&reactor)
            .await
            .map_err(|_| Failed)?;
        let logged = runtime::log::installed();
        let export = SessionPort::create().map_err(|_| Failed)?;

        let steps = PortChannel::client(steps, reactor.clone()).with_wait(Timeout::INFINITE);
        let mut fixture = Fixture {
            parent,
            export,
            held: None,
            reactor,
        };
        let mut last = (outcome::STARTED, u64::from(logged));
        loop {
            let (code, value) = last;
            let report = Report {
                outcome: code,
                value,
            };
            let step = steps.next(report).await.map_err(|_| Failed)?;
            match step.action {
                action::RETURN_OK => return Ok(()),
                action::RETURN_ERR => return Err(Failed),
                action::PANIC => panic_on_step(),
                _ => last = fixture.run(step).await,
            }
        }
    })
}

impl Fixture {
    async fn run(&mut self, step: Step) -> (u32, u64) {
        match step.action {
            action::ACCEPT => self.accept(Timeout::from_ns(step.argument)).await,
            action::ANSWER_SESSION => match self.export.create_session() {
                Ok(minted) => self.answer(Ok(minted.port)),
                Err(_) => (outcome::NO_SESSION, 0),
            },
            action::ANSWER_BADGED => {
                let copy = NonZeroU64::new(step.argument).and_then(|badge| {
                    self.export
                        .port()
                        .duplicate(Rights::WRITE | Rights::TRANSFER, Some(badge))
                        .ok()
                });
                match copy {
                    Some(port) => self.answer(Ok(port)),
                    None => (outcome::NO_SESSION, 0),
                }
            }
            action::ANSWER_REFUSAL => match session_error_of(step.argument) {
                Some(refusal) => self.answer(Err(refusal)),
                None => (outcome::UNKNOWN_STEP, 0),
            },
            action::DROP_REQUEST => match self.held.take() {
                Some(_) => (outcome::DROPPED, 0),
                None => (outcome::NO_REQUEST, 0),
            },
            action::RECV => self.recv(Timeout::from_ns(step.argument)).await,
            action::LOG => {
                log_line(STEP_LINE);
                (outcome::LOGGED, 0)
            }
            action::EXIT => process_exit(step.argument),
            action::WAIT => wait(),
            action::BOUND_FAILURE => match spawn_bound(|| Failed) {
                Ok(_) => wait(),
                Err(_) => (outcome::SPAWN_REFUSED, 0),
            },
            _ => (outcome::UNKNOWN_STEP, 0),
        }
    }

    async fn accept(&mut self, wait: Timeout) -> (u32, u64) {
        let parent =
            AsyncStartup::new(PortChannel::new(self.parent, self.reactor.clone())).with_wait(wait);

        match accept::<probe::Protocol>(&parent).await {
            Ok(request) => {
                self.held = Some(request);
                (outcome::ACCEPTED, 0)
            }
            Err(CallError::Remote(refusal)) => {
                (outcome::ACCEPT_REFUSED, accept_error_code(refusal))
            }
            Err(CallError::Ipc(error)) => (outcome::IPC_FAILED, ipc_error_code(error)),
        }
    }

    fn answer(&mut self, session: Result<Port, component_abi::SessionError>) -> (u32, u64) {
        match self.held.take() {
            Some(request) => (
                outcome::ANSWERED,
                u64::from(request.reply.send(session).is_ok()),
            ),
            None => (outcome::NO_REQUEST, 0),
        }
    }

    async fn recv(&self, wait: Timeout) -> (u32, u64) {
        let mut events = self.export.events();

        match self.reactor.timeout(wait, events.recv(&self.reactor)).await {
            Some(Ok(Some(component_startup::SessionEvent::Frame(frame)))) => {
                let badge = frame.resources.badge;
                let _ = probe::dispatch(&Echo, frame);
                (outcome::FRAME, badge)
            }
            Some(Ok(Some(component_startup::SessionEvent::Gone(badge)))) => {
                (outcome::SENDER_GONE, badge)
            }
            None => (outcome::RECV_TIMEOUT, 0),
            _ => (outcome::RECV_FAILED, 0),
        }
    }
}

#[expect(
    clippy::panic,
    reason = "the fixture checks the exit of a panicking component"
)]
fn panic_on_step() -> ! {
    panic!("{STEP_PANIC}")
}

fn wait() -> ! {
    if let Ok(own) = Port::create() {
        let _ = own.recv(Timeout::INFINITE);
    }
    exit(FAILED_EXIT_CODE)
}
