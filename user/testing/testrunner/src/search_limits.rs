//! Пределы состояния запросов rootkeeper: отказ `NoSpace` при исчерпании и возврат расхода.

use alloc::vec::Vec;

use component_abi::{STARTUP_REQUEST_TIMEOUT, SessionError, component_startup as startup_contract};
use component_startup::{Attributes, Found, Providers, StartupFailure};
use ipc::{CallError, wire::IpcError};
use kernel_objects::Session;
use rootkeeper::{RootSystem, SEARCH_LIMITS, SearchLimits};
use runtime::Timeout;
use syscall::Rights;

use crate::{
    composition::{Importer, pause},
    probe_protocol::probe,
    rootkeeper_system::{
        COMPONENT_REPLY_TIMEOUT, EXPORT, FILTERED, IDLE_EXPORT, IDLE_PROVIDER_REQUEST_TIMEOUT,
        NEIGHBOUR, POLL_ATTEMPTS, POLL_INTERVAL, PROBE, await_revoked, finish, limited, pair,
        probe_of, query, requested,
    },
};

const OWNER_STATES: SearchLimits = SearchLimits {
    owner_states: 2,
    ..SEARCH_LIMITS
};

const REFUSAL_REPEATS: usize = 4;

const UNDELIVERED_TIMEOUT: Timeout = Timeout::from_millis(50);

const NO_SPACE: CallError<SessionError> = CallError::Remote(SessionError::NoSpace);

#[test_harness::test]
fn searches_beyond_the_owner_limit_are_refused_and_another_owner_is_served() {
    let composition = limited(&[EXPORT], OWNER_STATES);
    let importer = composition.importer(&[probe_of(PROBE)]);
    let other = composition.importer(&[probe_of(PROBE)]);
    let _first = all(&importer).expect("first enumeration granted");
    let second = all(&importer).expect("second enumeration granted");

    let refused = (0..REFUSAL_REPEATS).all(|_| all(&importer).map(drop) == Err(NO_SPACE));
    let served = all(&other).map(drop);
    drop(second);
    let recovered = await_all(&importer).is_some();

    test_harness::check!(refused);
    test_harness::check_eq!(served, Ok(()));
    test_harness::check!(recovered);
    finish(composition);
}

#[test_harness::test]
fn an_enumeration_beyond_the_owner_memory_is_refused_whole() {
    let limits = SearchLimits {
        owner_bytes: RootSystem::enumeration_bytes(PROBE, 2) - 1,
        ..SEARCH_LIMITS
    };
    let composition = limited(&[EXPORT, NEIGHBOUR, FILTERED], limits);
    let importer = composition.importer(&[probe_of(PROBE)]);
    let bytes = query(&[pair("kind", "a")]);

    let wide = all(&importer).map(drop);
    let narrow = importer
        .enumeration::<probe::Protocol>(PROBE, Attributes::decode(&bytes).expect("query decodes"))
        .map(drop);

    test_harness::check_eq!(wide, Err(NO_SPACE));
    test_harness::check_eq!(narrow, Ok(()));
    finish(composition);
}

#[test_harness::test]
fn a_departing_owner_returns_the_shared_limit() {
    let limits = SearchLimits {
        total_states: 3,
        ..SEARCH_LIMITS
    };
    let composition = limited(&[EXPORT], limits);
    let importer = composition.importer(&[probe_of(PROBE)]);
    let other = composition.importer(&[probe_of(PROBE)]);
    let kept = all(&other).expect("independent enumeration granted");
    let enumeration = all(&importer).expect("enumeration granted");
    let found = next(&enumeration)
        .expect("next answered")
        .expect("a provider is found");

    let refused = all(&other).map(drop);
    drop(importer);
    let enumeration_revoked = await_revoked(|| enumeration.handle().duplicate(Rights::WRITE, None));
    let found_revoked = await_revoked(|| found.session.handle().duplicate(Rights::WRITE, None));
    let kept_found = await_next(&kept).is_some();
    let served = await_all(&other).is_some();

    test_harness::check_eq!(refused, Err(NO_SPACE));
    test_harness::check!(enumeration_revoked);
    test_harness::check!(found_revoked);
    test_harness::check!(kept_found);
    test_harness::check!(served);
    finish(composition);
}

#[test_harness::test]
fn a_result_beyond_the_owner_limit_keeps_its_place_in_the_enumeration() {
    let composition = limited(&[EXPORT, NEIGHBOUR], OWNER_STATES);
    let importer = composition.importer(&[probe_of(PROBE)]);
    let enumeration = all(&importer).expect("enumeration granted");
    let first = next(&enumeration)
        .expect("next answered")
        .expect("a provider is found");

    let refused_next = (0..REFUSAL_REPEATS)
        .all(|_| next(&enumeration).map(|found| found.is_some()) == Err(NO_SPACE));
    let refused_open = (0..REFUSAL_REPEATS).all(|_| opened(&first) == Err(NO_SPACE));
    drop(first);
    let second = await_next(&enumeration);
    let ended = next(&enumeration).map(|found| found.is_none());
    drop(enumeration);
    let second_opened = second.as_ref().is_some_and(await_opened);

    test_harness::check!(refused_next);
    test_harness::check!(refused_open);
    test_harness::check!(second.is_some());
    test_harness::check_eq!(ended, Ok(true));
    test_harness::check!(second_opened);
    finish(composition);
}

#[test_harness::test]
fn provider_sessions_continue_after_no_space() {
    let composition = limited(&[EXPORT, NEIGHBOUR], OWNER_STATES);
    let importer = composition.importer(&[probe_of(PROBE)]);
    let mut sessions = Providers::<probe::Protocol>::new(importer.handle(), PROBE)
        .all(Attributes::empty())
        .expect("enumeration granted");
    let first = sessions
        .next()
        .and_then(Result::ok)
        .expect("a provider is found");

    let refused = (0..REFUSAL_REPEATS).all(
        |_| matches!(sessions.next(), Some(Err(error)) if error.failure == StartupFailure::NoSpace),
    );
    drop(first);
    let second = (0..POLL_ATTEMPTS).find_map(|_| {
        let found = sessions.next().and_then(Result::ok);
        if found.is_none() {
            pause(POLL_INTERVAL);
        }
        found
    });

    test_harness::check!(refused);
    test_harness::check!(second.is_some());
    drop(second);
    finish(composition);
}

#[test_harness::test]
fn the_end_of_an_enumeration_returns_its_snapshot_memory() {
    let limits = SearchLimits {
        owner_bytes: RootSystem::enumeration_bytes(PROBE, 2)
            + 3 * RootSystem::enumeration_bytes(PROBE, 0),
        ..SEARCH_LIMITS
    };
    let composition = limited(&[EXPORT, NEIGHBOUR], limits);
    let importer = composition.importer(&[probe_of(PROBE)]);
    let enumeration = all(&importer).expect("enumeration granted");
    let found = [(); 2].map(|()| {
        next(&enumeration)
            .expect("next answered")
            .expect("a provider is found")
    });

    let before_end = all(&importer).map(drop);
    let ended = next(&enumeration).map(|found| found.is_none());
    let found_opened = found.iter().map(opened).collect::<Vec<_>>();
    let after_end = all(&importer).map(drop);

    test_harness::check_eq!(before_end, Err(NO_SPACE));
    test_harness::check_eq!(ended, Ok(true));
    test_harness::check_eq!(found_opened, [Ok(()), Ok(())]);
    test_harness::check_eq!(after_end, Ok(()));
    finish(composition);
}

#[test_harness::test]
fn waiting_session_requests_count_against_the_owner_limit() {
    let composition = limited(&[IDLE_EXPORT], OWNER_STATES);
    let importer = composition.importer(&[probe_of(PROBE)]);
    let other = composition.importer(&[probe_of(PROBE)]);

    let waiting = [(); 2]
        .map(|()| requested::<probe::Protocol>(&importer, PROBE, IDLE_PROVIDER_REQUEST_TIMEOUT));
    let refused = (0..REFUSAL_REPEATS).all(|_| {
        requested::<probe::Protocol>(&importer, PROBE, COMPONENT_REPLY_TIMEOUT) == Err(NO_SPACE)
    });
    let admitted = requested::<probe::Protocol>(&other, PROBE, IDLE_PROVIDER_REQUEST_TIMEOUT);

    let timed_out = Err(CallError::Ipc(IpcError::Timeout));
    test_harness::check_eq!(waiting, [timed_out, timed_out]);
    test_harness::check!(refused);
    test_harness::check_eq!(admitted, timed_out);
    finish(composition);
}

#[test_harness::test]
fn an_undelivered_result_keeps_its_export_and_returns_its_state() {
    let limits = SearchLimits {
        owner_states: 3,
        ..SEARCH_LIMITS
    };
    let composition = limited(&[EXPORT], limits);
    let importer = composition.importer(&[probe_of(PROBE)]);
    let enumeration = all(&importer).expect("enumeration granted");

    let undelivered = (0..REFUSAL_REPEATS).all(|_| {
        let held = composition.hold();
        let answer = component_startup::next(&enumeration, UNDELIVERED_TIMEOUT).map(drop);
        drop(held);
        answer == Err(CallError::Ipc(IpcError::Timeout))
    });
    let found = await_next(&enumeration).is_some();
    let recovered = await_all(&importer).is_some();

    test_harness::check!(undelivered);
    test_harness::check!(found);
    test_harness::check!(recovered);
    finish(composition);
}

#[test_harness::test]
fn watches_and_their_results_count_against_the_owner_limit() {
    let composition = limited(&[EXPORT], OWNER_STATES);
    let importer = composition.importer(&[probe_of(PROBE)]);
    let first = watch(&importer).expect("first watch granted");
    let second = watch(&importer).expect("second watch granted");

    let refused_watch = watch(&importer).map(drop);
    let refused_next = next(&first).map(|found| found.is_some());
    drop(second);
    let delivered = await_next(&first).is_some();

    test_harness::check_eq!(refused_watch, Err(NO_SPACE));
    test_harness::check_eq!(refused_next, Err(NO_SPACE));
    test_harness::check!(delivered);
    finish(composition);
}

fn all(
    importer: &Importer,
) -> Result<Session<startup_contract::Protocol>, CallError<SessionError>> {
    importer.enumeration::<probe::Protocol>(PROBE, Attributes::empty())
}

fn watch(
    importer: &Importer,
) -> Result<Session<startup_contract::Protocol>, CallError<SessionError>> {
    importer.watch::<probe::Protocol>(PROBE, Attributes::empty())
}

fn next(
    enumeration: &Session<startup_contract::Protocol>,
) -> Result<Option<Found>, CallError<SessionError>> {
    component_startup::next(enumeration, STARTUP_REQUEST_TIMEOUT)
}

fn opened(found: &Found) -> Result<(), CallError<SessionError>> {
    component_startup::open::<probe::Protocol>(found, COMPONENT_REPLY_TIMEOUT).map(drop)
}

fn await_all(importer: &Importer) -> Option<Session<startup_contract::Protocol>> {
    awaited(|| all(importer).ok())
}

fn await_next(enumeration: &Session<startup_contract::Protocol>) -> Option<Found> {
    awaited(|| next(enumeration).ok().flatten())
}

fn await_opened(found: &Found) -> bool {
    awaited(|| opened(found).ok()).is_some()
}

fn awaited<T>(attempt: impl Fn() -> Option<T>) -> Option<T> {
    (0..POLL_ATTEMPTS).find_map(|_| {
        let answer = attempt();
        if answer.is_none() {
            pause(POLL_INTERVAL);
        }
        answer
    })
}
