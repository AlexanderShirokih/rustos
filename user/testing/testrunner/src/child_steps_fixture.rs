#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::{num::NonZeroU64, panic::PanicInfo, sync::atomic::AtomicU64};

use component_abi::{SESSION_SCOPE_MAX, component_startup as startup_contract};
use component_startup::{SessionRequest, accept, component_manifest, mint_session};
use ipc::{
    Listener as _, RecvError,
    wire::{MESSAGE_INLINE_MAX, Str},
};
use kernel_objects::Port;
use runtime::{PortTransport, Timeout, log_line, process_exit, start_component};
use syscall::{Handle, Rights, kind};

mod child_steps_protocol;
mod probe_protocol;

use child_steps_protocol::{
    Report, STEP_LINE, Step, accept_error_code, action, child_steps, ipc_error_code, outcome,
    session_error_of,
};
use probe_protocol::{Echo, probe};

const LOG_TAG: &str = "steps";

const FAILED_EXIT_CODE: u64 = 1;

component_manifest! {
    struct StepsStartup {
        requirements: { steps: session(child_steps::Protocol) },
        exports: [probe::Protocol],
    }
}

struct Fixture {
    parent: Handle<kind::Port>,
    export: Port,
    next_badge: AtomicU64,
    held: Option<SessionRequest>,
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(parent: Handle<kind::Port>) -> ! {
    start_component(parent, LOG_TAG);
    let Ok(requirements) = StepsStartup::request(parent) else {
        process_exit(FAILED_EXIT_CODE)
    };
    let logged = runtime::log::installed();
    let Ok(export) = Port::create() else {
        process_exit(FAILED_EXIT_CODE)
    };

    let steps = requirements.steps.client();
    let mut fixture = Fixture {
        parent,
        export,
        next_badge: AtomicU64::new(0),
        held: None,
    };
    let mut last = (outcome::STARTED, u64::from(logged));
    loop {
        let (code, value) = last;
        let scope = match &fixture.held {
            Some(request) if code == outcome::ACCEPTED => request.scope.as_str(),
            _ => "",
        };
        let Some(scope) = Str::<SESSION_SCOPE_MAX>::new(scope) else {
            process_exit(FAILED_EXIT_CODE)
        };
        let report = Report {
            outcome: code,
            value,
            scope,
        };
        let Ok(step) = steps.next(report) else {
            process_exit(FAILED_EXIT_CODE)
        };
        last = fixture.run(step);
    }
}

impl Fixture {
    fn run(&mut self, step: Step) -> (u32, u64) {
        match step.action {
            action::ACCEPT => self.accept(Timeout::from_ns(step.argument)),
            action::ANSWER_SESSION => match mint_session(&self.export, &self.next_badge) {
                Ok(minted) => self.answer(Ok(minted.port)),
                Err(_) => (outcome::NO_SESSION, 0),
            },
            action::ANSWER_BADGED => {
                let copy = NonZeroU64::new(step.argument).and_then(|badge| {
                    self.export
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
            action::RECV => self.recv(Timeout::from_ns(step.argument)),
            action::LOG => {
                log_line(STEP_LINE);
                (outcome::LOGGED, 0)
            }
            action::EXIT => process_exit(step.argument),
            action::WAIT => wait(),
            _ => (outcome::UNKNOWN_STEP, 0),
        }
    }

    fn accept(&mut self, wait: Timeout) -> (u32, u64) {
        let parent = startup_contract::Client::new(PortTransport::new(self.parent)).with_wait(wait);
        match accept::<probe::Protocol>(&parent) {
            Ok(Ok(request)) => {
                self.held = Some(request);
                (outcome::ACCEPTED, 0)
            }
            Ok(Err(refusal)) => (outcome::ACCEPT_REFUSED, accept_error_code(refusal)),
            Err(error) => (outcome::IPC_FAILED, ipc_error_code(error)),
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

    fn recv(&self, wait: Timeout) -> (u32, u64) {
        let mut bytes = [0u8; MESSAGE_INLINE_MAX];
        match PortTransport::new(self.export.as_raw()).recv(&mut bytes, wait) {
            Ok(frame) => {
                let badge = frame.resources.badge;
                let _ = probe::dispatch(&mut Echo, frame);
                (outcome::FRAME, badge)
            }
            Err(RecvError::SenderGone(badge)) => (outcome::SENDER_GONE, badge),
            Err(RecvError::Timeout) => (outcome::RECV_TIMEOUT, 0),
            Err(_) => (outcome::RECV_FAILED, 0),
        }
    }
}

fn wait() -> ! {
    if let Ok(own) = Port::create() {
        let _ = own.recv(Timeout::INFINITE);
    }
    process_exit(FAILED_EXIT_CODE)
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    process_exit(FAILED_EXIT_CODE)
}
