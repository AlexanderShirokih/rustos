//! E2E relay логирования: лог-порт с сессиями по значкам вместо bootstrap-капы
//! и граница полномочий выданной лог-сессии.
//!
//! Строки relay сценария копит сток `Recording` над стоком сценария. Сток
//! сценария делает встречный двусторонний вызов ядра. Вызов перезаписывает значок
//! в IPC-буфере потока, и источник записанной строки проверяет, что relay снял
//! значок до пересылки.

use alloc::{string::String, sync::Arc, vec::Vec};
use core::{
    num::NonZeroU64,
    sync::atomic::{AtomicBool, Ordering::SeqCst},
};

use bootstrap_abi::bootstrap;
use ipc::wire::{Header, IpcError, MessageBuf};
use kernel_objects::{Port, Session};
use log_abi::{LOG_MESSAGE_MAX, LOG_SOURCE_MAX, LogBuffer, log};
use log_relay::{KernelLogSink, LogRelayThread, LogSink, SourceNames};
use rootkeeper::relay::LogSessions;
use runtime::{PortTransport, Timeout, port_send};
use spin::Mutex;
use syscall::{HANDLE_TABLE_CAPACITY, Rights, SyscallError};

use crate::{
    fixture::spawn_entry, ipc_raw, log_fixture_protocol::FIXTURE_LINE,
    resource_exhaustion::exhaust_handle_table,
};

const NAMED_BADGE: NonZeroU64 = NonZeroU64::new(0x0101_0101_0101_0101).expect("badge is non-zero");
const UNNAMED_BADGE: NonZeroU64 =
    NonZeroU64::new(0x0202_0202_0202_0202).expect("badge is non-zero");

pub(crate) const SELFTEST_BADGE: NonZeroU64 =
    NonZeroU64::new(0x0400_0000).expect("badge is non-zero");
pub(crate) const C_SELFTEST_BADGE: NonZeroU64 =
    NonZeroU64::new(0x0400_0001).expect("badge is non-zero");
pub(crate) const STEPS_BADGE: NonZeroU64 = NonZeroU64::new(0x0400_0002).expect("badge is non-zero");
pub(crate) const CXX_SELFTEST_BADGE: NonZeroU64 =
    NonZeroU64::new(0x0400_0003).expect("badge is non-zero");
pub(crate) const CXX_EXIT_BADGE: NonZeroU64 =
    NonZeroU64::new(0x0400_0004).expect("badge is non-zero");
pub(crate) const SQLITE_SELFTEST_BADGE: NonZeroU64 =
    NonZeroU64::new(0x0400_0005).expect("badge is non-zero");

pub(crate) const STEPS_SOURCE: &str = "child-steps";

const SHARED_SOURCES: [(NonZeroU64, &str); 6] = [
    (SELFTEST_BADGE, "std-selftest"),
    (C_SELFTEST_BADGE, "c-selftest"),
    (STEPS_BADGE, STEPS_SOURCE),
    (CXX_SELFTEST_BADGE, "cxx-selftest"),
    (CXX_EXIT_BADGE, "cxx-exit"),
    (SQLITE_SELFTEST_BADGE, "sqlite-selftest"),
];

pub(crate) struct SharedSourceNames;

struct TestSourceNames;

impl SourceNames for SharedSourceNames {
    fn name(&self, badge: u64) -> Option<LogBuffer<LOG_SOURCE_MAX>> {
        SHARED_SOURCES
            .iter()
            .find(|(selftest, _)| selftest.get() == badge)
            .map(|(_, name)| source(name))
    }
}

impl SourceNames for TestSourceNames {
    fn name(&self, badge: u64) -> Option<LogBuffer<LOG_SOURCE_MAX>> {
        (badge == NAMED_BADGE.get()).then(|| source("named-subsystem"))
    }
}

fn source(name: &str) -> LogBuffer<LOG_SOURCE_MAX> {
    let mut buffer = LogBuffer::new();
    buffer.push_str(name);
    buffer
}

const LOG_CLIENT_FIXTURE: &str = "/log-client-fixture/exec";

const RELAY_SOURCE: &str = "log-relay";

const RECORD_TIMEOUT: Timeout = Timeout::from_secs(3);
const RECORD_POLL: Timeout = Timeout::from_millis(10);
const JOIN_TIMEOUT: Timeout = Timeout::from_secs(5);
const SEND_TIMEOUT: Timeout = Timeout::from_millis(500);

const SINK_FAILURE_LIMIT: usize = log_relay::SINK_FAILURE_LIMIT as usize;

const CHURN_BATCH: u64 = 64;

const SYNC_BADGE_BASE: NonZeroU64 = NonZeroU64::new(0x0300_0000).expect("badge is non-zero");

struct Relay {
    thread: LogRelayThread,
    lines: Lines,
}

type Lines = Arc<Mutex<Vec<RelayLine>>>;

#[derive(Clone)]
struct RelayLine {
    source: String,
    text: String,
}

struct Recording<S> {
    lines: Lines,
    next: S,
}

static SHARED: Mutex<Option<Relay>> = Mutex::new(None);

static ACTIVE: Mutex<Option<Relay>> = Mutex::new(None);

static SINK_REFUSES: AtomicBool = AtomicBool::new(false);

pub(crate) struct SharedLogSessions;

pub(crate) struct ScenarioLogSessions;

impl Relay {
    fn start(
        names: impl SourceNames + 'static,
        sink: impl LogSink + Send + 'static,
    ) -> kernel_objects::Result<Self> {
        let lines = Lines::default();
        let recording = Recording {
            lines: Arc::clone(&lines),
            next: sink,
        };
        let thread = LogRelayThread::start(names, recording)?;
        Ok(Self { thread, lines })
    }
}

impl<S: LogSink> LogSink for Recording<S> {
    fn write(&mut self, source: &str, message: &str) -> Result<(), IpcError> {
        let written = self.next.write(source, message);
        // Тесты отказа стока считают и отвергнутые строки.
        self.lines.lock().push(RelayLine {
            source: String::from(source),
            text: String::from(message),
        });
        written
    }
}

impl LogSessions for SharedLogSessions {
    fn session(&self, badge: NonZeroU64) -> Option<Session<log::Protocol>> {
        with_shared(|relay| relay.thread.session(badge).ok())
    }
}

impl LogSessions for ScenarioLogSessions {
    fn session(&self, badge: NonZeroU64) -> Option<Session<log::Protocol>> {
        ACTIVE.lock().as_ref()?.thread.session(badge).ok()
    }
}

fn with_shared<T>(body: impl FnOnce(&Relay) -> T) -> T {
    let mut shared = SHARED.lock();
    let relay = shared.get_or_insert_with(|| {
        let sink = KernelLogSink::new(crate::bootstrap_handle());
        Relay {
            thread: LogRelayThread::start(SharedSourceNames, sink)
                .expect("shared log relay starts"),
            lines: Lines::default(),
        }
    });
    body(relay)
}

pub(crate) fn shutdown_relays() {
    stop(&ACTIVE);
    stop(&SHARED);
}

fn stop(slot: &Mutex<Option<Relay>>) {
    // Путь завершения прогона не ждёт лока. Ожидание превращает провал теста в зависание.
    if let Some(mut held) = slot.try_lock()
        && let Some(relay) = held.take()
    {
        let _ = relay.thread.shutdown(JOIN_TIMEOUT);
    }
}

struct ScenarioSink;

impl LogSink for ScenarioSink {
    fn write(&mut self, source: &str, message: &str) -> Result<(), IpcError> {
        let kernel = bootstrap::Client::new(PortTransport::new(crate::bootstrap_handle()));
        let forwarded = kernel.log_str(source, message);
        // Двусторонний вызов ядра перезаписывает значок в IPC-буфере потока.
        // Выданный образ закрывает ответ: значок буфера здесь важнее самой capability.
        let _ = kernel.acquire_userland_image();
        if SINK_REFUSES.load(SeqCst) {
            return Err(IpcError::PeerClosed);
        }
        forwarded
    }
}

pub(crate) fn start_component_relay(names: impl SourceNames + 'static) {
    SINK_REFUSES.store(false, SeqCst);
    let relay = Relay::start(names, ScenarioSink).expect("component log relay starts");
    *ACTIVE.lock() = Some(relay);
}

pub(crate) fn await_component_sources(names: &[&str]) -> Vec<String> {
    await_records(|records| {
        names
            .iter()
            .all(|name| records.iter().any(|record| record.source == *name))
    })
    .into_iter()
    .map(|record| record.source)
    .collect()
}

pub(crate) fn await_component_lines(name: &str, count: usize) -> Vec<String> {
    await_component_lines_with_prefix(name, "", count)
}

pub(crate) fn await_component_lines_with_prefix(
    name: &str,
    prefix: &str,
    count: usize,
) -> Vec<String> {
    await_records(|records| {
        records
            .iter()
            .filter(|record| record.source == name && record.text.starts_with(prefix))
            .count()
            >= count
    })
    .into_iter()
    .filter(|record| record.source == name)
    .map(|record| record.text)
    .collect()
}

fn await_records(complete: impl Fn(&[RelayLine]) -> bool) -> Vec<RelayLine> {
    let idle = Port::create().expect("port create");
    let mut waited = Timeout::POLL;
    loop {
        let records = scenario_lines();
        if complete(&records) || waited >= RECORD_TIMEOUT {
            return records;
        }
        let _ = idle.recv(RECORD_POLL);
        waited = waited.saturating_add(RECORD_POLL);
    }
}

fn scenario_lines() -> Vec<RelayLine> {
    ACTIVE
        .lock()
        .as_ref()
        .map(|relay| relay.lines.lock().clone())
        .unwrap_or_default()
}

pub(crate) fn stop_component_relay() {
    stop(&ACTIVE);
}

fn scenario_session(badge: NonZeroU64) -> Port {
    // Лок ACTIVE не удерживается за пределами минта.
    ACTIVE
        .lock()
        .as_ref()
        .expect("scenario relay is running")
        .thread
        .session(badge)
        .expect("log session")
        .into_handle()
}

fn with_relay(body: impl FnOnce(&dyn Fn(NonZeroU64) -> Port)) {
    start_component_relay(TestSourceNames);

    body(&scenario_session);

    SINK_REFUSES.store(false, SeqCst);
    ACTIVE
        .lock()
        .take()
        .expect("scenario relay is running")
        .thread
        .shutdown(JOIN_TIMEOUT)
        .expect("test log relay stops");
}

fn wait_for_records(count: usize) -> Vec<RelayLine> {
    let mut records = await_records(|records| {
        records
            .iter()
            .filter(|record| record.source != RELAY_SOURCE)
            .count()
            >= count
    });
    records.retain(|record| record.source != RELAY_SOURCE);
    test_harness::check!(records.len() >= count);
    records
}

fn write_log(session: &Port, message: &str) {
    log::Client::new(PortTransport::new(session))
        .write_str(message)
        .expect("log frame delivered");
}

fn send_unknown_ordinal(session: &Port) {
    let mut frame = MessageBuf::<()>::new();
    frame
        .write_header(&Header::new(log::write::ORDINAL ^ 1))
        .expect("header fits");
    frame.finish().expect("frame fits");
    ipc_raw::write_frame(frame.as_bytes(), &[]);
    test_harness::check_eq!(port_send(session.as_raw(), SEND_TIMEOUT), 0);
}

fn send_log_frame_with_cap(session: &Port, cap: Port) -> i64 {
    let mut frame = MessageBuf::<()>::new();
    frame
        .write_header(&Header::new(log::write::ORDINAL))
        .expect("header fits");
    frame
        .write_field(1, "frame with a capability".as_bytes())
        .expect("message fits");
    frame.finish().expect("frame fits");
    // Ядро вынимает capability из таблицы отправителя при рандеву: владение
    // хэндлом отдаётся кадру.
    ipc_raw::write_frame(frame.as_bytes(), &[cap.as_raw().raw()]);
    let sent = port_send(session.as_raw(), SEND_TIMEOUT);
    if sent >= 0 {
        let _ = cap.into_raw();
    }
    sent
}

#[test_harness::test]
fn relay_forwards_a_frame_under_the_badge_name() {
    with_relay(|session| {
        let logger = session(NAMED_BADGE);
        write_log(&logger, "hello");

        let records = wait_for_records(1);
        test_harness::check_eq!(records.len(), 1);
        test_harness::check!(records[0].source == "named-subsystem");
        test_harness::check!(records[0].text == "hello");
    });
}

#[test_harness::test]
fn relay_names_an_unnamed_badge_numerically() {
    with_relay(|session| {
        let logger = session(UNNAMED_BADGE);
        write_log(&logger, "line");

        let records = wait_for_records(1);
        test_harness::check!(records[0].source == "badge-0202020202020202");
    });
}

#[test_harness::test]
fn relay_escapes_control_characters_in_the_message() {
    with_relay(|session| {
        let logger = session(NAMED_BADGE);
        write_log(&logger, "first\r\n\tsecond\u{1b}[2J\u{0}\u{85}");

        let records = wait_for_records(1);
        let expected = "first\\r\\n\\tsecond\\u{1b}[2J\\u{0}\\u{85}";
        test_harness::check!(records[0].text == expected);
    });
}

#[test_harness::test]
fn relay_escapes_unicode_line_and_paragraph_separators() {
    with_relay(|session| {
        let logger = session(NAMED_BADGE);
        write_log(&logger, "a\u{2028}b\u{2029}c");

        let records = wait_for_records(1);
        let expected = "a\\u{2028}b\\u{2029}c";
        test_harness::check!(records[0].text == expected);
    });
}

#[test_harness::test]
fn relay_escapes_unicode_format_controls() {
    with_relay(|session| {
        let logger = session(NAMED_BADGE);
        write_log(&logger, "c\u{2066}d");

        let records = wait_for_records(1);
        let expected = "c\\u{2066}d";
        test_harness::check!(records[0].text == expected);
    });
}

#[test_harness::test]
fn relay_splits_a_grown_message_without_losing_its_tail() {
    with_relay(|session| {
        let logger = session(NAMED_BADGE);
        let message = "\u{1b}".repeat(LOG_MESSAGE_MAX);
        write_log(&logger, &message);

        let escaped_character_len = "\\u{1b}".len();
        let characters_per_record = LOG_MESSAGE_MAX / escaped_character_len;
        let record_count = LOG_MESSAGE_MAX.div_ceil(characters_per_record);
        let records = wait_for_records(record_count);
        let joined: String = records.iter().map(|record| record.text.as_str()).collect();
        let expected = "\\u{1b}".repeat(LOG_MESSAGE_MAX);
        test_harness::check!(joined == expected);
        test_harness::check!(
            records
                .iter()
                .all(|record| record.text.len() <= LOG_MESSAGE_MAX)
        );
    });
}

#[test_harness::test]
fn relay_keeps_ordinary_utf8_unchanged() {
    with_relay(|session| {
        let logger = session(NAMED_BADGE);
        write_log(&logger, "こんにちは, 世界!");

        let records = wait_for_records(1);
        test_harness::check!(records[0].text == "こんにちは, 世界!");
    });
}

#[test_harness::test]
fn relay_never_writes_an_unprefixed_physical_line() {
    with_relay(|session| {
        let logger = session(NAMED_BADGE);
        write_log(&logger, "ok\n\u{1b}[31mforged");

        let records = wait_for_records(1);
        test_harness::check!(records[0].source == "named-subsystem");
        test_harness::check!(!records[0].text.chars().any(char::is_control));
    });
}

#[test_harness::test]
fn relay_forwards_an_empty_message_once() {
    with_relay(|session| {
        let logger = session(NAMED_BADGE);
        write_log(&logger, "");

        let records = wait_for_records(1);
        test_harness::check_eq!(records.len(), 1);
        test_harness::check!(records[0].text.is_empty());
    });
}

#[test_harness::test]
fn relay_serves_past_a_departed_session() {
    with_relay(|session| {
        let departing = session(UNNAMED_BADGE);
        write_log(&departing, "before departure");
        wait_for_records(1);
        drop(departing);

        let logger = session(NAMED_BADGE);
        write_log(&logger, "after departure");
        let records = wait_for_records(2);
        test_harness::check!(records[1].text == "after departure");
    });
}

#[test_harness::test]
fn relay_rejects_one_bad_frame_and_serves_the_next() {
    with_relay(|session| {
        let logger = session(NAMED_BADGE);
        send_unknown_ordinal(&logger);
        write_log(&logger, "after a bad frame");

        let records = wait_for_records(1);
        test_harness::check!(records[0].text == "after a bad frame");
    });
}

#[test_harness::test]
fn relay_closes_capabilities_carried_by_a_log_frame() {
    with_relay(|session| {
        let logger = session(NAMED_BADGE);
        let carried = Port::create().expect("carried port");
        let writer = carried
            .duplicate(Rights::WRITE | Rights::TRANSFER, None)
            .expect("writer side");
        test_harness::check_eq!(send_log_frame_with_cap(&logger, carried), 0);
        wait_for_records(1);

        test_harness::check_eq!(
            port_send(writer.as_raw(), SEND_TIMEOUT),
            SyscallError::Revoked.as_return_value()
        );
    });
}

#[test_harness::test]
fn relay_serves_after_receiver_resource_exhaustion() {
    with_relay(|session| {
        let logger = session(NAMED_BADGE);
        let (_source, mut fillers) = exhaust_handle_table();
        let carried = fillers.pop().expect("full table contains filler handles");

        test_harness::check_eq!(
            send_log_frame_with_cap(&logger, carried),
            SyscallError::OutOfHandles.as_return_value()
        );
        for _ in 0..2 {
            drop(fillers.pop().expect("full table contains filler handles"));
        }
        write_log(&logger, "after resource exhaustion");

        let records = wait_for_records(1);
        test_harness::check!(records[0].text == "after resource exhaustion");
    });
}

#[test_harness::test]
fn log_session_denies_bootstrap_operations() {
    with_relay(|session| {
        let fixture = spawn_entry(LOG_CLIENT_FIXTURE, session(NAMED_BADGE));
        fixture.join(JOIN_TIMEOUT).expect("fixture terminates");
        test_harness::check_eq!(fixture.exit_code().expect("exit code"), 0);

        let records = wait_for_records(1);
        test_harness::check!(records[0].text == FIXTURE_LINE);

        let next = session(NAMED_BADGE);
        write_log(&next, "relay is alive");
        let records = wait_for_records(2);
        test_harness::check!(records[1].text == "relay is alive");
    });
}

#[test_harness::test]
fn relay_drains_departed_badges_so_new_sessions_mint() {
    with_relay(|session| {
        let mut minted = 0;
        let mut records = 0;
        while minted < u64::from(HANDLE_TABLE_CAPACITY) + CHURN_BATCH {
            for badge in minted..minted + CHURN_BATCH {
                drop(session(NonZeroU64::MIN.saturating_add(badge)));
            }
            minted += CHURN_BATCH;
            let sync = session(SYNC_BADGE_BASE.saturating_add(minted));
            write_log(&sync, "session after churn");
            records += 1;
            wait_for_records(records);
        }
    });
}

#[test_harness::test]
fn relay_stops_when_its_log_port_closes() {
    let relay =
        LogRelayThread::start(TestSourceNames, ScenarioSink).expect("test log relay starts");
    relay.shutdown(JOIN_TIMEOUT).expect("test log relay stops");
}

#[test_harness::test]
fn relay_stops_a_frame_at_the_first_refused_chunk() {
    with_relay(|session| {
        let logger = session(NAMED_BADGE);
        let message = "\u{1b}".repeat(LOG_MESSAGE_MAX);

        SINK_REFUSES.store(true, SeqCst);
        write_log(&logger, &message);
        let records = wait_for_records(1);
        test_harness::check_eq!(records.len(), 1);

        SINK_REFUSES.store(false, SeqCst);
        write_log(&logger, "accepted after refused frame");
        let records = wait_for_records(2);
        test_harness::check!(records[1].text == "accepted after refused frame");
    });
}

#[test_harness::test]
fn relay_stops_after_the_sink_refuses_in_a_row() {
    with_relay(|session| {
        let logger = session(NAMED_BADGE);

        SINK_REFUSES.store(true, SeqCst);
        for _ in 1..SINK_FAILURE_LIMIT {
            write_log(&logger, "refused");
        }
        SINK_REFUSES.store(false, SeqCst);
        write_log(&logger, "accepted");
        let records = wait_for_records(SINK_FAILURE_LIMIT);
        test_harness::check!(records[SINK_FAILURE_LIMIT - 1].text == "accepted");

        SINK_REFUSES.store(true, SeqCst);
        for _ in 0..SINK_FAILURE_LIMIT {
            write_log(&logger, "refused");
        }
        wait_for_records(2 * SINK_FAILURE_LIMIT);
        SINK_REFUSES.store(false, SeqCst);

        test_harness::check_eq!(
            port_send(logger.as_raw(), SEND_TIMEOUT),
            SyscallError::Timeout.as_return_value()
        );
    });
}
