//! Библиотека родителя на стартовом порту: заявки импортёров, уход и перезапуск детей, журнал
//! детей и глубина ресурса родителя.

use alloc::{vec, vec::Vec};
use core::num::NonZeroU64;

use component_abi::SessionError;
use component_startup::{RequestError, STARTUP_REQUEST_TIMEOUT};
use ipc::{
    Listener as _, Protocol as _, RecvError,
    wire::{IpcError, MESSAGE_INLINE_MAX},
};
use kernel_objects::{Delivery, Error, IssuedHandle, Port, Resource, Session, kind};
use managed_component::{Departure, GrantError, Outcome, Status};
use runtime::{Instant, JoinHandle, PortTransport, Priority, Timeout, port_send, spawn_owned};
use syscall::{MAX_RESOURCE_DEPTH, ProcessName, Rights, SyscallError};

use crate::{
    child_steps_protocol::{
        Report, Step, child_steps, ipc_error_code, outcome, session_error_code,
    },
    composition::{pause, request},
    ipc_raw,
    parent::{Answer, Child, Parent},
    probe_protocol::probe,
    startup_exit_protocol::{
        FIRST_IPC_FAILED_EXIT_CODE, PANIC_EXIT_CODE, SECOND_SESSION_REFUSED_EXIT_CODE,
    },
};

type Debt = IssuedHandle<kind::Reply>;

const STEPS_ENTRY: &str = "/child-steps-fixture/exec";
const IMPORTER_ENTRY: &str = "/probe-importer-fixture/exec";
const TWO_REQUIREMENTS_ENTRY: &str = "/two-requirements-fixture/exec";
const HELPER_PANIC_ENTRY: &str = "/helper-panic-fixture/exec";
const CLOSED_ENTRY: &str = "/managed-startup-closed/exec";
const EXPORT_ENTRY: &str = "/export-fixture/exec";

const PROVIDER: &str = "provider";
const SECOND_PROVIDER: &str = "second-provider";
const IMPORTER: &str = "importer";
const TWO_REQUIREMENTS: &str = "two-requirements";
const HELPER_PANIC: &str = "helper-panic";
const CLOSED: &str = "closed";

const STEPS_REQUIREMENT: &str = "steps";
const PROBE_REQUIREMENT: &str = "probe";
const FIRST_REQUIREMENT: &str = "first";
const SECOND_REQUIREMENT: &str = "second";

const STEP_REPORT_TIMEOUT: Timeout = Timeout::from_secs(5);
const NO_EVENT_TIMEOUT: Timeout = Timeout::from_millis(100);
const SENDER_GONE_TIMEOUT: Timeout = Timeout::from_secs(1);
const DEPARTURE_TIMEOUT: Timeout = Timeout::from_secs(5);
const UNANSWERED_IMPORTER_DEPARTURE_TIMEOUT: Timeout = Timeout::from_secs(10);
const NO_DEPARTURE_TIMEOUT: Timeout = Timeout::from_millis(200);
const NO_REPORT_TIMEOUT: Timeout = Timeout::from_millis(200);
const NO_ACCEPT_PAUSE: Timeout = Timeout::from_millis(500);
const IMPORTER_THREAD_TIMEOUT: Timeout = Timeout::from_secs(5);
const OUTSIDER_REFUSAL_TIMEOUT: Timeout = Timeout::from_secs(1);
const TAKE_DOWN_DURATION_MAX: Timeout = Timeout::from_secs(1);
const REVOKED_SEND_TIMEOUT: Timeout = Timeout::from_millis(500);

const WORKER_STACK_SIZE: u64 = 0x8000;

const FAILURE_EXIT_CODE: u32 = 5;
const READ_EXIT_CODE: u32 = 7;
const TERMINATE_EXIT_CODE: u32 = 9;

const SESSION_BADGE: u64 = 0x4242;
const OUTSIDER_BADGE: NonZeroU64 = NonZeroU64::new(0x7700_0000).expect("badge is non-zero");

const PING: u64 = 0x5A;

const GRANTED: u32 = 0;

const DEPTH_SIX: u16 = 6;
const DEPTH_SEVEN: u16 = 7;
const DEPTH_BUDGET: u64 = 2 * 1024 * 1024;
const PROBE_BYTES: u64 = 1;

struct Steps {
    port: Port,
    waiting: Option<child_steps::next::Reply<Debt>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Reported {
    outcome: u32,
    value: u64,
}

#[derive(Default)]
struct Received {
    report: Option<Reported>,
    reply: Option<child_steps::next::Reply<Debt>>,
}

impl child_steps::Service<Delivery> for Received {
    fn next(&mut self, _frame: &Delivery, report: Report, reply: child_steps::next::Reply<Debt>) {
        self.report = Some(Reported {
            outcome: report.outcome,
            value: report.value,
        });
        self.reply = Some(reply);
    }
}

impl Steps {
    fn new() -> Self {
        Self {
            port: Port::create().expect("steps port"),
            waiting: None,
        }
    }

    fn child(&self, name: &'static str) -> Child {
        let source = self
            .port
            .duplicate(Rights::WRITE | Rights::TRANSFER | Rights::DUPLICATE, None)
            .expect("steps source");
        Child::new(STEPS_ENTRY, name)
            .requirement(
                STEPS_REQUIREMENT,
                Answer::Grant {
                    protocol: child_steps::Protocol::ID,
                    source,
                },
            )
            .export::<probe::Protocol>()
    }

    fn report(&mut self, wait: Timeout) -> Option<Reported> {
        let mut bytes = [0u8; MESSAGE_INLINE_MAX];
        loop {
            match PortTransport::new(self.port.as_raw()).recv(&mut bytes, wait) {
                Ok(incoming) => {
                    let mut received = Received::default();
                    let _ = child_steps::dispatch(&mut received, incoming);
                    if let (Some(report), Some(reply)) = (received.report, received.reply) {
                        self.waiting = Some(reply);
                        return Some(report);
                    }
                }
                Err(RecvError::SenderGone(_) | RecvError::Discarded) => {}
                Err(RecvError::Timeout | RecvError::Closed) => return None,
            }
        }
    }

    fn next(&mut self) -> Reported {
        self.report(STEP_REPORT_TIMEOUT)
            .expect("the fixture reports its step")
    }

    fn send(&mut self, step: Step) {
        self.waiting
            .take()
            .expect("the fixture waits for a step")
            .send(step)
            .expect("step delivered");
    }

    fn run(&mut self, step: Step) -> Reported {
        self.send(step);
        self.next()
    }
}

fn report(outcome: u32, value: u64) -> Reported {
    Reported { outcome, value }
}

fn started(logged: bool) -> Reported {
    report(outcome::STARTED, u64::from(logged))
}

fn accepted() -> Reported {
    Reported {
        outcome: outcome::ACCEPTED,
        value: 0,
    }
}

fn answered(delivered: bool) -> Reported {
    report(outcome::ANSWERED, u64::from(delivered))
}

fn step_failed(error: IpcError) -> Reported {
    report(outcome::IPC_FAILED, ipc_error_code(error))
}

const fn departed(child: u16, outcome: Outcome) -> Departure<u16> {
    Departure { child, outcome }
}

fn asked(result: &Result<Session<probe::Protocol>, RequestError>) -> u32 {
    let code = match result {
        Ok(_) => u64::from(GRANTED),
        Err(RequestError::Parent(refusal)) => 100 + session_error_code(*refusal),
        Err(RequestError::Ipc(error)) => 200 + ipc_error_code(*error),
    };
    u32::try_from(code).expect("request code fits u32")
}

fn refused(refusal: SessionError) -> u32 {
    asked(&Err(RequestError::Parent(refusal)))
}

fn failed(error: IpcError) -> u32 {
    asked(&Err(RequestError::Ipc(error)))
}

fn ask(importer: &Port, requirement: &'static str) -> JoinHandle {
    let importer = importer.as_raw();
    spawn_owned(
        move || {
            asked(&request::<probe::Protocol>(
                importer,
                requirement,
                Timeout::INFINITE,
            ))
        },
        WORKER_STACK_SIZE,
        Priority::new(1),
    )
    .expect("importer thread starts")
}

fn finished(worker: JoinHandle) -> u32 {
    worker
        .wait(IMPORTER_THREAD_TIMEOUT)
        .expect("importer thread ends");
    let code = worker.exit_code().expect("importer thread exit code");
    worker.join(Timeout::POLL).expect("importer thread joins");
    code
}

fn probe_importer(parent: &Parent, provider: u16) -> Port {
    parent.importer(vec![(
        PROBE_REQUIREMENT,
        Answer::export::<probe::Protocol>(provider),
    )])
}

fn own_depth() -> u16 {
    let mut chain = vec![Resource::self_resource().expect("self metering resource")];
    loop {
        let next = chain.last().expect("chain is non-empty").split(PROBE_BYTES);
        match next {
            Ok(next) => chain.push(next),
            Err(_) => break,
        }
    }
    MAX_RESOURCE_DEPTH - u16::try_from(chain.len() - 1).expect("chain length fits u16")
}

fn resource_chain(depth: u16) -> Vec<Resource> {
    let own = own_depth();
    test_harness::check!(own <= depth);
    let mut chain = vec![Resource::self_resource().expect("self metering resource")];
    for _ in own..depth {
        let next = chain
            .last()
            .expect("chain is non-empty")
            .split(DEPTH_BUDGET)
            .expect("split within the depth ceiling");
        chain.push(next);
    }
    chain
}

#[test_harness::test]
fn an_undelivered_request_waits_for_the_next_accept() {
    let parent = Parent::start();
    let mut steps = Steps::new();
    let provider = parent.add(steps.child(PROVIDER));
    test_harness::check_eq!(steps.next(), started(true));

    test_harness::check_eq!(
        steps.run(Step::accept(NO_EVENT_TIMEOUT)),
        step_failed(IpcError::Timeout)
    );
    let importer = probe_importer(&parent, provider);
    let asking = ask(&importer, PROBE_REQUIREMENT);
    test_harness::check!(parent.await_requests(1));
    test_harness::check_eq!(steps.run(Step::accept(Timeout::INFINITE)), accepted());
    test_harness::check_eq!(steps.run(Step::answer_session()), answered(true));
    test_harness::check_eq!(finished(asking), GRANTED);
}

#[test_harness::test]
fn a_session_the_importer_did_not_receive_is_closed_by_the_provider() {
    let parent = Parent::start();
    let mut steps = Steps::new();
    let provider = parent.add(steps.child(PROVIDER));
    test_harness::check_eq!(steps.next(), started(true));
    let importer = probe_importer(&parent, provider);

    test_harness::check_eq!(
        asked(&request::<probe::Protocol>(
            importer.as_raw(),
            PROBE_REQUIREMENT,
            NO_EVENT_TIMEOUT
        )),
        failed(IpcError::Timeout)
    );
    test_harness::check_eq!(steps.run(Step::accept(Timeout::INFINITE)), accepted());
    test_harness::check_eq!(
        steps.run(Step::answer_badged(SESSION_BADGE)),
        answered(false)
    );
    test_harness::check_eq!(
        steps.run(Step::recv(SENDER_GONE_TIMEOUT)),
        report(outcome::SENDER_GONE, SESSION_BADGE)
    );
}

#[test_harness::test]
fn a_provider_refusal_reaches_the_importer() {
    let parent = Parent::start();
    let mut steps = Steps::new();
    let provider = parent.add(steps.child(PROVIDER));
    test_harness::check_eq!(steps.next(), started(true));
    let importer = probe_importer(&parent, provider);

    let asking = ask(&importer, PROBE_REQUIREMENT);
    test_harness::check_eq!(steps.run(Step::accept(Timeout::INFINITE)), accepted());
    test_harness::check_eq!(
        steps.run(Step::answer_refusal(SessionError::Refused)),
        answered(true)
    );
    test_harness::check_eq!(finished(asking), refused(SessionError::Refused));
}

#[test_harness::test]
fn a_dropped_request_ends_the_importer_wait() {
    let parent = Parent::start();
    let mut steps = Steps::new();
    let provider = parent.add(steps.child(PROVIDER));
    test_harness::check_eq!(steps.next(), started(true));
    let importer = probe_importer(&parent, provider);

    let asking = ask(&importer, PROBE_REQUIREMENT);
    test_harness::check_eq!(steps.run(Step::accept(Timeout::INFINITE)), accepted());
    test_harness::check_eq!(steps.run(Step::drop_request()), report(outcome::DROPPED, 0));
    test_harness::check_eq!(finished(asking), failed(IpcError::PeerClosed));
}

#[test_harness::test]
fn a_repeated_session_request_takes_the_place_of_the_waiting_one() {
    let parent = Parent::start();
    let mut steps = Steps::new();
    let provider = parent.add(steps.child(PROVIDER));
    test_harness::check_eq!(steps.next(), started(true));
    let importer = probe_importer(&parent, provider);

    test_harness::check_eq!(
        asked(&request::<probe::Protocol>(
            importer.as_raw(),
            PROBE_REQUIREMENT,
            NO_EVENT_TIMEOUT
        )),
        failed(IpcError::Timeout)
    );
    let repeat = ask(&importer, PROBE_REQUIREMENT);
    test_harness::check!(parent.await_requests(2));
    test_harness::check_eq!(steps.run(Step::accept(Timeout::INFINITE)), accepted());
    test_harness::check_eq!(steps.run(Step::answer_session()), answered(true));
    test_harness::check_eq!(finished(repeat), GRANTED);
    test_harness::check_eq!(
        steps.run(Step::accept(NO_EVENT_TIMEOUT)),
        step_failed(IpcError::Timeout)
    );
}

#[test_harness::test]
fn a_session_request_replaced_by_a_repeat_is_answered_with_no_space() {
    let parent = Parent::start();
    let mut steps = Steps::new();
    let provider = parent.add(steps.child(PROVIDER));
    test_harness::check_eq!(steps.next(), started(true));
    let importer = probe_importer(&parent, provider);

    let first = ask(&importer, PROBE_REQUIREMENT);
    test_harness::check!(parent.await_requests(1));
    let second = ask(&importer, PROBE_REQUIREMENT);
    test_harness::check!(parent.await_requests(2));
    test_harness::check_eq!(finished(first), refused(SessionError::NoSpace));

    test_harness::check_eq!(steps.run(Step::accept(Timeout::INFINITE)), accepted());
    test_harness::check_eq!(steps.run(Step::answer_session()), answered(true));
    test_harness::check_eq!(finished(second), GRANTED);
}

#[test_harness::test]
fn a_repeated_accept_takes_the_place_of_the_waiting_one() {
    let parent = Parent::start();
    let mut steps = Steps::new();
    let provider = parent.add(steps.child(PROVIDER));
    test_harness::check_eq!(steps.next(), started(true));
    let importer = probe_importer(&parent, provider);

    test_harness::check_eq!(
        steps.run(Step::accept(NO_EVENT_TIMEOUT)),
        step_failed(IpcError::Timeout)
    );
    steps.send(Step::accept(Timeout::INFINITE));
    test_harness::check!(parent.await_accepts(2));
    let asking = ask(&importer, PROBE_REQUIREMENT);
    test_harness::check_eq!(steps.next(), accepted());
    test_harness::check_eq!(steps.run(Step::answer_session()), answered(true));
    test_harness::check_eq!(finished(asking), GRANTED);
}

#[test_harness::test]
fn the_departure_of_an_importer_removes_its_waiting_request() {
    let parent = Parent::start();
    let mut steps = Steps::new();
    let provider = parent.add(steps.child(PROVIDER));
    test_harness::check_eq!(steps.next(), started(true));

    let importer = parent.add(Child::new(IMPORTER_ENTRY, IMPORTER).requirement(
        PROBE_REQUIREMENT,
        Answer::export::<probe::Protocol>(provider),
    ));
    test_harness::check!(parent.await_requests(1));
    parent.terminate(importer, READ_EXIT_CODE);
    test_harness::check_eq!(
        parent.departure(DEPARTURE_TIMEOUT),
        Some(departed(importer, Outcome::Exit(READ_EXIT_CODE)))
    );
    test_harness::check_eq!(
        steps.run(Step::accept(NO_EVENT_TIMEOUT)),
        step_failed(IpcError::Timeout)
    );
}

#[test_harness::test]
fn a_waiting_request_outlives_the_departure_of_the_provider() {
    let parent = Parent::start();
    let mut steps = Steps::new();
    let provider = parent.add(steps.child(PROVIDER));
    test_harness::check_eq!(steps.next(), started(true));
    let importer = probe_importer(&parent, provider);

    let asking = ask(&importer, PROBE_REQUIREMENT);
    test_harness::check!(parent.await_requests(1));
    steps.send(Step::exit(FAILURE_EXIT_CODE));
    test_harness::check_eq!(
        parent.departure(DEPARTURE_TIMEOUT),
        Some(departed(provider, Outcome::Exit(FAILURE_EXIT_CODE)))
    );

    parent.restart(provider);
    test_harness::check_eq!(steps.next(), started(true));
    test_harness::check_eq!(steps.run(Step::accept(Timeout::INFINITE)), accepted());
    test_harness::check_eq!(steps.run(Step::answer_session()), answered(true));
    test_harness::check_eq!(finished(asking), GRANTED);
}

#[test_harness::test]
fn a_session_request_lost_with_the_provider_is_repeated_by_the_start() {
    let parent = Parent::start();
    let mut steps = Steps::new();
    let provider = parent.add(steps.child(PROVIDER));
    test_harness::check_eq!(steps.next(), started(true));
    let importer = parent.add(Child::new(IMPORTER_ENTRY, IMPORTER).requirement(
        PROBE_REQUIREMENT,
        Answer::export::<probe::Protocol>(provider),
    ));

    test_harness::check_eq!(steps.run(Step::accept(Timeout::INFINITE)), accepted());
    steps.send(Step::exit(FAILURE_EXIT_CODE));
    test_harness::check_eq!(
        parent.departure(DEPARTURE_TIMEOUT),
        Some(departed(provider, Outcome::Exit(FAILURE_EXIT_CODE)))
    );
    test_harness::check!(parent.await_requests(2));

    parent.restart(provider);
    test_harness::check_eq!(steps.next(), started(true));
    test_harness::check_eq!(steps.run(Step::accept(Timeout::INFINITE)), accepted());
    test_harness::check_eq!(steps.run(Step::answer_session()), answered(true));
    test_harness::check_eq!(parent.departure(NO_DEPARTURE_TIMEOUT), None);
    test_harness::check_eq!(parent.status(importer), Some(Status::Running));
}

#[test_harness::test]
fn an_accept_is_paired_only_with_requests_routed_to_its_provider() {
    let parent = Parent::start();
    let mut routed = Steps::new();
    let mut other = Steps::new();
    let provider = parent.add(routed.child(PROVIDER));
    parent.add(other.child(SECOND_PROVIDER));
    test_harness::check_eq!(routed.next(), started(true));
    test_harness::check_eq!(other.next(), started(true));
    let importer = probe_importer(&parent, provider);

    let asking = ask(&importer, PROBE_REQUIREMENT);
    test_harness::check!(parent.await_requests(1));
    test_harness::check_eq!(
        other.run(Step::accept(NO_EVENT_TIMEOUT)),
        step_failed(IpcError::Timeout)
    );
    test_harness::check_eq!(routed.run(Step::accept(Timeout::INFINITE)), accepted());
    test_harness::check_eq!(routed.run(Step::answer_session()), answered(true));
    test_harness::check_eq!(finished(asking), GRANTED);
}

#[test_harness::test]
fn an_unexpected_call_on_the_startup_port_refuses_its_caller() {
    let parent = Parent::start();
    let outsider = parent.outsider(OUTSIDER_BADGE);

    test_harness::check_eq!(
        probe::Client::new(PortTransport::new(&outsider))
            .with_wait(OUTSIDER_REFUSAL_TIMEOUT)
            .ping(PING),
        Err(IpcError::PeerClosed)
    );
    test_harness::check_eq!(
        asked(&request::<probe::Protocol>(
            outsider.as_raw(),
            PROBE_REQUIREMENT,
            OUTSIDER_REFUSAL_TIMEOUT
        )),
        refused(SessionError::Refused)
    );
}

#[test_harness::test]
fn a_refused_requirement_ends_the_start_and_keeps_the_earlier_capabilities() {
    let parent = Parent::start();
    let mut steps = Steps::new();
    let provider = parent.add(steps.child(PROVIDER));
    test_harness::check_eq!(steps.next(), started(true));

    let two = parent.add(
        Child::new(TWO_REQUIREMENTS_ENTRY, TWO_REQUIREMENTS)
            .requirement(
                FIRST_REQUIREMENT,
                Answer::export::<probe::Protocol>(provider),
            )
            .requirement(SECOND_REQUIREMENT, Answer::Refuse),
    );
    test_harness::check_eq!(steps.run(Step::accept(Timeout::INFINITE)), accepted());
    test_harness::check_eq!(
        steps.run(Step::answer_badged(SESSION_BADGE)),
        answered(true)
    );
    test_harness::check_eq!(
        parent.departure(DEPARTURE_TIMEOUT),
        Some(departed(
            two,
            Outcome::Exit(SECOND_SESSION_REFUSED_EXIT_CODE)
        ))
    );
    test_harness::check_eq!(
        steps.run(Step::recv(SENDER_GONE_TIMEOUT)),
        report(outcome::SENDER_GONE, SESSION_BADGE)
    );
    test_harness::check_eq!(
        steps.run(Step::recv(NO_EVENT_TIMEOUT)),
        report(outcome::RECV_TIMEOUT, 0)
    );
}

#[test_harness::test]
fn an_unanswered_requirement_ends_the_start_after_the_request_timeout() {
    let parent = Parent::start();
    let mut steps = Steps::new();
    let provider = parent.add(steps.child(PROVIDER));
    test_harness::check_eq!(steps.next(), started(true));

    let since = Instant::now();
    let two = parent.add(
        Child::new(TWO_REQUIREMENTS_ENTRY, TWO_REQUIREMENTS)
            .requirement(
                FIRST_REQUIREMENT,
                Answer::export::<probe::Protocol>(provider),
            )
            .requirement(
                SECOND_REQUIREMENT,
                Answer::export::<probe::Protocol>(provider),
            ),
    );
    test_harness::check_eq!(
        parent.departure(UNANSWERED_IMPORTER_DEPARTURE_TIMEOUT),
        Some(departed(two, Outcome::Exit(FIRST_IPC_FAILED_EXIT_CODE)))
    );
    test_harness::check!(Instant::now().since(since).raw() >= STARTUP_REQUEST_TIMEOUT.raw());
    test_harness::check_eq!(parent.requests(), 1);
}

#[test_harness::test]
fn a_restart_of_a_running_child_changes_nothing() {
    let parent = Parent::start();
    let mut steps = Steps::new();
    let provider = parent.add(steps.child(PROVIDER));
    test_harness::check_eq!(steps.next(), started(true));
    let badge = parent.badge(provider);
    test_harness::check!(badge.is_some());

    parent.restart(provider);
    test_harness::check_eq!(steps.report(NO_REPORT_TIMEOUT), None);
    test_harness::check_eq!(
        steps.run(Step::accept(NO_EVENT_TIMEOUT)),
        step_failed(IpcError::Timeout)
    );
    test_harness::check_eq!(parent.badge(provider), badge);
    test_harness::check_eq!(parent.status(provider), Some(Status::Running));
}

#[test_harness::test]
fn the_exit_of_an_earlier_launch_is_reported_once_and_spares_the_next_launch() {
    let parent = Parent::start();
    let mut steps = Steps::new();
    let provider = parent.add(steps.child(PROVIDER));
    test_harness::check_eq!(steps.next(), started(true));

    steps.send(Step::exit(FAILURE_EXIT_CODE));
    test_harness::check_eq!(
        parent.departure(DEPARTURE_TIMEOUT),
        Some(departed(provider, Outcome::Exit(FAILURE_EXIT_CODE)))
    );
    parent.restart(provider);
    test_harness::check_eq!(steps.next(), started(true));
    test_harness::check_eq!(parent.departure(NO_DEPARTURE_TIMEOUT), None);
    test_harness::check_eq!(steps.run(Step::log()), report(outcome::LOGGED, 0));
    test_harness::check_eq!(parent.status(provider), Some(Status::Running));
}

#[test_harness::test]
fn a_child_terminated_by_its_parent_departs_with_the_named_code() {
    let parent = Parent::start();
    let mut steps = Steps::new();
    let provider = parent.add(steps.child(PROVIDER));
    test_harness::check_eq!(steps.next(), started(true));

    parent.terminate(provider, TERMINATE_EXIT_CODE);
    test_harness::check_eq!(
        parent.departure(DEPARTURE_TIMEOUT),
        Some(departed(provider, Outcome::Exit(TERMINATE_EXIT_CODE)))
    );
    parent.terminate(provider, TERMINATE_EXIT_CODE);
    test_harness::check_eq!(parent.status(provider), Some(Status::Departed));
    test_harness::check_eq!(parent.departure(NO_DEPARTURE_TIMEOUT), None);
}

#[test_harness::test]
fn a_living_child_that_closes_its_start_session_is_terminated() {
    let parent = Parent::start();
    let closed = parent.add(Child::new(CLOSED_ENTRY, CLOSED));

    test_harness::check_eq!(
        parent.departure(DEPARTURE_TIMEOUT),
        Some(departed(closed, Outcome::TakenDown))
    );
    test_harness::check!(parent.last_depart().raw() < TAKE_DOWN_DURATION_MAX.raw());
}

#[test_harness::test]
fn a_panic_in_a_helper_thread_ends_the_child_process() {
    let parent = Parent::start();
    let helper = parent.add(Child::new(HELPER_PANIC_ENTRY, HELPER_PANIC));

    test_harness::check_eq!(
        parent.departure(DEPARTURE_TIMEOUT),
        Some(departed(helper, Outcome::Exit(PANIC_EXIT_CODE)))
    );
}

#[test_harness::test]
fn a_child_whose_parent_refuses_log_starts_without_it() {
    let parent = Parent::start();
    let mut steps = Steps::new();
    let provider = parent.add(steps.child(PROVIDER).log(Answer::Refuse));

    test_harness::check_eq!(steps.next(), started(false));
    steps.send(Step::exit(0));
    test_harness::check_eq!(
        parent.departure(DEPARTURE_TIMEOUT),
        Some(departed(provider, Outcome::Exit(0)))
    );
}

#[test_harness::test]
fn a_log_session_of_a_departed_incarnation_is_revoked() {
    let parent = Parent::start();
    let mut steps = Steps::new();
    let provider = parent.add(steps.child(PROVIDER));
    test_harness::check_eq!(steps.next(), started(true));
    let copy = parent
        .log_copy(provider)
        .expect("log copy of the incarnation");

    parent.terminate(provider, TERMINATE_EXIT_CODE);
    test_harness::check_eq!(
        parent.departure(DEPARTURE_TIMEOUT),
        Some(departed(provider, Outcome::Exit(TERMINATE_EXIT_CODE)))
    );
    ipc_raw::write_frame(&[], &[]);
    test_harness::check_eq!(
        port_send(copy.as_raw(), REVOKED_SEND_TIMEOUT),
        SyscallError::Revoked.as_return_value()
    );
}

#[test_harness::test]
fn an_idle_provider_calls_accept_once() {
    let parent = Parent::start();
    let mut steps = Steps::new();
    let provider = parent.add(steps.child(PROVIDER));
    test_harness::check_eq!(steps.next(), started(true));

    steps.send(Step::accept(Timeout::INFINITE));
    parent.add(Child::new(IMPORTER_ENTRY, IMPORTER).requirement(
        PROBE_REQUIREMENT,
        Answer::export::<probe::Protocol>(provider),
    ));
    test_harness::check_eq!(steps.next(), accepted());
    test_harness::check_eq!(steps.run(Step::answer_session()), answered(true));
    steps.send(Step::wait());

    pause(NO_ACCEPT_PAUSE);
    test_harness::check_eq!(parent.accepts(), 1);
    test_harness::check_eq!(parent.requests(), 1);
}

#[test_harness::test]
fn a_parent_at_resource_depth_six_restarts_its_child() {
    let chain = resource_chain(DEPTH_SIX);
    let parent = Parent::start();
    let mut steps = Steps::new();
    let provider = parent
        .add_on(
            steps.child(PROVIDER).budget(DEPTH_BUDGET),
            chain.last().expect("chain is non-empty"),
        )
        .expect("record at depth six");
    test_harness::check_eq!(steps.next(), started(true));

    steps.send(Step::exit(FAILURE_EXIT_CODE));
    test_harness::check_eq!(
        parent.departure(DEPARTURE_TIMEOUT),
        Some(departed(provider, Outcome::Exit(FAILURE_EXIT_CODE)))
    );
    parent.restart(provider);
    test_harness::check_eq!(steps.next(), started(true));
}

#[test_harness::test]
fn a_parent_at_resource_depth_seven_is_refused_a_child_record() {
    let chain = resource_chain(DEPTH_SEVEN);
    let resource = chain.last().expect("chain is non-empty");
    let parent = Parent::start();
    let steps = Steps::new();

    test_harness::check!(matches!(
        parent.add_on(steps.child(PROVIDER).budget(DEPTH_BUDGET), resource),
        Err(Error::Syscall(SyscallError::ResourceExhausted))
    ));
    test_harness::check!(resource.split(DEPTH_BUDGET).is_ok());
}

#[test_harness::test]
fn a_repeated_key_in_add_is_refused() {
    let resource = Resource::self_resource().expect("self metering resource");
    let program = crate::fixture::program(EXPORT_ENTRY);
    let budget = crate::fixture::process_bytes(&program);
    let mut children = managed_component::Children::new().expect("startup port created");
    children
        .add(
            0u16,
            program,
            ProcessName::new(PROVIDER).expect("fixture process name"),
            &resource,
            budget,
            Timeout::from_secs(1),
        )
        .expect("first record added");

    test_harness::check!(matches!(
        children.add(
            0u16,
            program,
            ProcessName::new(PROVIDER).expect("fixture process name"),
            &resource,
            budget,
            Timeout::from_secs(1)
        ),
        Err(Error::Syscall(SyscallError::InvalidArgument))
    ));
}

#[test_harness::test]
fn next_launch_is_infinite_without_a_waiting_record_and_poll_with_a_fresh_one() {
    let resource = Resource::self_resource().expect("self metering resource");
    let program = crate::fixture::program(EXPORT_ENTRY);
    let budget = crate::fixture::process_bytes(&program);
    let mut children = managed_component::Children::new().expect("startup port created");
    test_harness::check_eq!(children.next_launch(), Timeout::INFINITE);

    children
        .add(
            0u16,
            program,
            ProcessName::new(PROVIDER).expect("fixture process name"),
            &resource,
            budget,
            Timeout::from_secs(1),
        )
        .expect("record added");
    test_harness::check_eq!(children.next_launch(), Timeout::POLL);
}

#[test_harness::test]
fn a_grant_of_another_kind_on_a_granted_requirement_is_wrong_type() {
    let parent = Parent::start();
    let mut steps = Steps::new();
    let provider = parent.add(steps.child(PROVIDER));
    test_harness::check_eq!(steps.next(), started(true));

    let mismatched = parent.log_copy_as_resource(provider);

    test_harness::check!(matches!(
        mismatched,
        Err(GrantError::Kernel(Error::Syscall(SyscallError::WrongType)))
    ));
}
