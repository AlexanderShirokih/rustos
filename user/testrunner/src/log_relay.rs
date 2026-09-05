//! E2E relay логирования: лог-порт с сессиями по значкам вместо bootstrap-капы
//! и граница полномочий выданной лог-сессии.
//!
//! Сток тестового relay записывает пересланные строки и делает встречный
//! двусторонний вызов ядра: он перезаписывает значок в IPC-буфере потока, и
//! записанный источник проверяет, что relay снял значок до пересылки.

use alloc::{
    string::{String, ToString},
    vec::Vec,
};
use core::{
    num::NonZeroU64,
    sync::atomic::{AtomicBool, Ordering::SeqCst},
};

use bootstrap_abi::bootstrap;
use ipc::wire::{Header, IpcError, MESSAGE_INLINE_MAX, MessageBuf};
use kernel_objects::{MemoryRegion, OwnedHandle, Port, Process, Resource};
use kernel_tests::kernel_test;
use log_abi::{LOG_MESSAGE_MAX, LOG_SOURCE_MAX, LogBuffer, log};
use log_relay::{KernelLogSink, LogRelayThread, LogSink, SourceNames};
use runtime::{PortTransport, Timeout, port_send};
use spin::Mutex;
use syscall::{HANDLE_TABLE_CAPACITY, Rights, SyscallError};
use userland_image::{component_name, decode};
use userland_loader::load_entry;

use crate::{ipc_raw, log_fixture_protocol::FIXTURE_LINE};

pub(crate) const ROUTER_BADGE: NonZeroU64 =
    NonZeroU64::new(0x1001_1001_1001_1001).expect("badge is non-zero");

const NAMED_BADGE: NonZeroU64 = NonZeroU64::new(0x0101_0101_0101_0101).expect("badge is non-zero");
const UNNAMED_BADGE: NonZeroU64 =
    NonZeroU64::new(0x0202_0202_0202_0202).expect("badge is non-zero");

pub(crate) const SELFTEST_BADGE: NonZeroU64 =
    NonZeroU64::new(0x0400_0000).expect("badge is non-zero");

struct SharedSourceNames;

struct TestSourceNames;

impl SourceNames for SharedSourceNames {
    fn name(&self, badge: u64) -> Option<LogBuffer<LOG_SOURCE_MAX>> {
        match badge {
            _ if badge == ROUTER_BADGE.get() => Some(source("filesystem")),
            _ if badge == SELFTEST_BADGE.get() => Some(source("std-selftest")),
            _ => None,
        }
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

const LOG_CLIENT_FIXTURE: &str = "/boot/log-client-fixture/program";

const RELAY_SOURCE: &str = "log-relay";

const RECORD_TIMEOUT: Timeout = Timeout::from_secs(3);
const RECORD_POLL: Timeout = Timeout::from_millis(10);
const JOIN_TIMEOUT: Timeout = Timeout::from_secs(5);
const SEND_TIMEOUT: Timeout = Timeout::from_millis(500);

const SINK_FAILURE_LIMIT: usize = log_relay::SINK_FAILURE_LIMIT as usize;

const CHURN_BATCH: u64 = 64;

const SYNC_BADGE_BASE: NonZeroU64 = NonZeroU64::new(0x0300_0000).expect("badge is non-zero");

#[derive(Clone)]
struct Record {
    source: String,
    message: String,
}

static RECORDED: Mutex<Vec<Record>> = Mutex::new(Vec::new());

static SHARED: Mutex<Option<LogRelayThread>> = Mutex::new(None);

static ACTIVE: Mutex<Option<LogRelayThread>> = Mutex::new(None);

static SINK_REFUSES: AtomicBool = AtomicBool::new(false);

pub(crate) fn shared_relay_thread() -> Option<syscall::Handle> {
    SHARED.lock().as_ref().map(LogRelayThread::handle)
}

pub(crate) fn scenario_relay_thread() -> Option<syscall::Handle> {
    ACTIVE.lock().as_ref().map(LogRelayThread::handle)
}

pub(crate) fn component_log_session(badge: NonZeroU64) -> Option<OwnedHandle> {
    let mut shared = SHARED.lock();
    let relay = shared.get_or_insert_with(|| {
        LogRelayThread::start(
            SharedSourceNames,
            KernelLogSink::new(crate::bootstrap_handle()),
        )
        .expect("shared log relay starts")
    });
    relay.delegated_session(badge).ok()
}

pub(crate) fn shutdown_relays() {
    stop(&ACTIVE);
    stop(&SHARED);
}

fn stop(slot: &Mutex<Option<LogRelayThread>>) {
    // Путь завершения прогона не ждёт лока: ожидание превращает провал теста в зависание.
    if let Some(mut held) = slot.try_lock()
        && let Some(relay) = held.take()
    {
        let _ = relay.shutdown(JOIN_TIMEOUT);
    }
}

struct RecordingSink;

impl LogSink for RecordingSink {
    fn write(&mut self, source: &str, message: &str) -> Result<(), IpcError> {
        let kernel = bootstrap::Client::new(PortTransport::client(crate::bootstrap_handle()));
        let forwarded = kernel.log_str(source, message);
        // Двусторонний вызов ядра перезаписывает значок в IPC-буфере потока.
        // Выданный образ закрывает ответ: значок буфера здесь важнее самой capability.
        let _ = kernel.acquire_userland_image();
        RECORDED.lock().push(Record {
            source: source.to_string(),
            message: message.to_string(),
        });
        if SINK_REFUSES.load(SeqCst) {
            return Err(IpcError::PeerClosed);
        }
        forwarded
    }
}

pub(crate) fn start_component_relay() {
    // Имена источников ставит и читает сам rootkeeper: сценарий подменяет
    // только сток, чтобы прочитать пересланные строки.
    RECORDED.lock().clear();
    SINK_REFUSES.store(false, SeqCst);
    *ACTIVE.lock() = Some(
        LogRelayThread::start(rootkeeper::relay::ComponentNames, RecordingSink)
            .expect("component log relay starts"),
    );
}

pub(crate) fn component_scenario_session(badge: NonZeroU64) -> Option<OwnedHandle> {
    ACTIVE.lock().as_ref()?.delegated_session(badge).ok()
}

pub(crate) fn await_component_sources(names: &[&str]) -> Vec<String> {
    let idle = Port::create().expect("port create");
    let mut waited = 0;
    loop {
        let sources: Vec<String> = RECORDED
            .lock()
            .iter()
            .map(|record| record.source.clone())
            .collect();
        let complete = names.iter().all(|name| {
            sources
                .iter()
                .any(|source| source.split('/').next() == Some(name))
        });
        if complete || waited >= RECORD_TIMEOUT.raw() {
            return sources;
        }
        let _ = idle.recv(RECORD_POLL);
        waited += RECORD_POLL.raw();
    }
}

pub(crate) fn stop_component_relay() {
    stop(&ACTIVE);
    rootkeeper::relay::install_names(core::iter::empty());
}

fn scenario_session(badge: NonZeroU64) -> OwnedHandle {
    // Лок ACTIVE не удерживается за пределами минта.
    ACTIVE
        .lock()
        .as_ref()
        .expect("scenario relay is running")
        .session(badge)
        .expect("log session")
}

fn with_relay(body: impl FnOnce(&dyn Fn(NonZeroU64) -> OwnedHandle)) {
    RECORDED.lock().clear();
    SINK_REFUSES.store(false, SeqCst);
    *ACTIVE.lock() =
        Some(LogRelayThread::start(TestSourceNames, RecordingSink).expect("test log relay starts"));

    body(&scenario_session);

    SINK_REFUSES.store(false, SeqCst);
    ACTIVE
        .lock()
        .take()
        .expect("scenario relay is running")
        .shutdown(JOIN_TIMEOUT)
        .expect("test log relay stops");
}

fn wait_for_records(count: usize) -> Vec<Record> {
    // Порт-таймер заводится один на всё ожидание: приёмник ему не нужен.
    let idle = Port::create().expect("port create");
    let mut waited = 0;
    loop {
        let records: Vec<Record> = RECORDED
            .lock()
            .iter()
            .filter(|record| record.source != RELAY_SOURCE)
            .cloned()
            .collect();
        if records.len() >= count {
            return records;
        }
        kernel_tests::kassert!(waited < RECORD_TIMEOUT.raw());
        let _ = idle.recv(RECORD_POLL);
        waited += RECORD_POLL.raw();
    }
}

fn write_log(session: &OwnedHandle, tag: &str, message: &str) {
    log::Client::new(PortTransport::client(session))
        .write_str(tag, message)
        .expect("log frame delivered");
}

fn send_unknown_ordinal(session: &OwnedHandle) {
    let mut frame = MessageBuf::<{ MESSAGE_INLINE_MAX }>::new();
    frame
        .write_header(&Header::new(log::write::ORDINAL ^ 1, 0, 0))
        .expect("header fits");
    frame.finish().expect("frame fits");
    ipc_raw::write_frame(frame.as_bytes(), &[]);
    kernel_tests::kassert_eq!(port_send(session.as_raw(), SEND_TIMEOUT), 0);
}

fn send_log_frame_with_cap(session: &OwnedHandle, cap: OwnedHandle) -> i64 {
    let mut frame = MessageBuf::<{ MESSAGE_INLINE_MAX }>::new();
    frame
        .write_header(&Header::new(log::write::ORDINAL, 0, 0))
        .expect("header fits");
    frame
        .write_field(1, "carrier".as_bytes())
        .expect("tag fits");
    frame
        .write_field(2, "frame with a capability".as_bytes())
        .expect("message fits");
    frame.finish().expect("frame fits");
    // Ядро вынимает capability из таблицы отправителя при рандеву: владение
    // хэндлом отдаётся кадру.
    let carried = cap.into_raw();
    ipc_raw::write_frame(frame.as_bytes(), &[carried.raw()]);
    let sent = port_send(session.as_raw(), SEND_TIMEOUT);
    if sent < 0 {
        drop(OwnedHandle::from_handle(carried));
    }
    sent
}

fn exhaust_handle_table() -> (Port, Vec<OwnedHandle>) {
    let source = Port::create().expect("filler port");
    let mut handles = Vec::new();
    loop {
        match source.handle().duplicate(Rights::TRANSFER, None) {
            Ok(handle) => handles.push(handle),
            Err(error) => {
                kernel_tests::kassert!(matches!(
                    error,
                    kernel_objects::Error::Syscall(SyscallError::OutOfHandles)
                ));
                break;
            }
        }
    }
    (source, handles)
}

fn spawn_image_entry(path: &str, start: OwnedHandle) -> Process {
    let kernel = bootstrap::Client::new(PortTransport::client(crate::bootstrap_handle()));
    let mut response = kernel
        .acquire_userland_image()
        .expect("acquire_userland_image call");
    let capability = response.value.expect("image vend");
    let cap = response.claim(capability).expect("capability owner");
    let region = MemoryRegion::from_handle(OwnedHandle::from(cap));
    let mapping = region.map_full_read_only().expect("map image read-only");
    let image = decode(mapping.as_bytes()).expect("decode userland image");
    let program = image
        .entry(path)
        .expect("image carries the entry")
        .program()
        .expect("entry record carries a program");
    let resource = Resource::self_resource().expect("self metering resource");
    load_entry(&program, Some(component_name(path)), &resource, start).expect("spawn from image")
}

// Кадр лог-сессии доходит до стока под именем её значка, а локальный тег
// клиента остаётся его суффиксом.
#[kernel_test]
fn relay_forwards_a_frame_under_the_trusted_source() {
    with_relay(|session| {
        let logger = session(NAMED_BADGE);
        write_log(&logger, "local", "hello");

        let records = wait_for_records(1);
        kernel_tests::kassert_eq!(records.len(), 1);
        kernel_tests::kassert!(records[0].source == "named-subsystem/local");
        kernel_tests::kassert!(records[0].message == "hello");
    });
}

// Пустой локальный тег оставляет источником одно имя значка.
#[kernel_test]
fn relay_forwards_an_untagged_frame_under_the_badge_name() {
    with_relay(|session| {
        let logger = session(NAMED_BADGE);
        write_log(&logger, "", "untagged");

        let records = wait_for_records(1);
        kernel_tests::kassert!(records[0].source == "named-subsystem");
    });
}

// Значок вне таблицы имён попадает в журнал числовой формой, а клиентский тег
// остаётся суффиксом и не даёт выдать себя за чужую сессию.
#[kernel_test]
fn relay_names_an_unnamed_badge_numerically() {
    with_relay(|session| {
        let logger = session(UNNAMED_BADGE);
        write_log(&logger, "named-subsystem", "spoof attempt");

        let records = wait_for_records(1);
        kernel_tests::kassert!(records[0].source == "badge-0202020202020202/named-subsystem");
    });
}

// Переводы строк, табуляция и terminal controls доходят до стока только
// видимыми escape-последовательностями.
#[kernel_test]
fn relay_escapes_control_characters_in_the_message() {
    with_relay(|session| {
        let logger = session(NAMED_BADGE);
        write_log(&logger, "local", "first\r\n\tsecond\u{1b}[2J\u{0}\u{85}");

        let records = wait_for_records(1);
        let expected = "first\\r\\n\\tsecond\\u{1b}[2J\\u{0}\\u{85}";
        kernel_tests::kassert!(records[0].message == expected);
    });
}

// Клиентская часть source не меняет его скобочную структуру и не создаёт
// физической строки.
#[kernel_test]
fn relay_escapes_control_characters_and_brackets_in_the_local_tag() {
    with_relay(|session| {
        let logger = session(NAMED_BADGE);
        write_log(&logger, "left[inner]\r\nright", "line");

        let records = wait_for_records(1);
        kernel_tests::kassert!(records[0].source == "named-subsystem/left\\[inner\\]\\r\\nright");
    });
}

// Unicode-разделители строк и абзацев нормализуются независимо от свойства
// `char::is_control`.
#[kernel_test]
fn relay_escapes_unicode_line_and_paragraph_separators() {
    with_relay(|session| {
        let logger = session(NAMED_BADGE);
        write_log(&logger, "local", "a\u{2028}b\u{2029}c");

        let records = wait_for_records(1);
        let expected = "a\\u{2028}b\\u{2029}c";
        kernel_tests::kassert!(records[0].message == expected);
    });
}

// Unicode format controls не могут переставить доверенный source или текст
// сообщения при отображении журнала.
#[kernel_test]
fn relay_escapes_unicode_format_controls() {
    with_relay(|session| {
        let logger = session(NAMED_BADGE);
        write_log(&logger, "a\u{202e}b", "c\u{2066}d");

        let records = wait_for_records(1);
        let source = "named-subsystem/a\\u{202e}b";
        let message = "c\\u{2066}d";
        kernel_tests::kassert!(records[0].source == source);
        kernel_tests::kassert!(records[0].message == message);
    });
}

// Рост строки после escaping порождает несколько bounded-записей без потери
// хвоста нормализованного сообщения.
#[kernel_test]
fn relay_splits_a_grown_message_without_losing_its_tail() {
    with_relay(|session| {
        let logger = session(NAMED_BADGE);
        let message = "\u{1b}".repeat(LOG_MESSAGE_MAX);
        write_log(&logger, "local", &message);

        let escaped_character_len = "\\u{1b}".len();
        let characters_per_record = LOG_MESSAGE_MAX / escaped_character_len;
        let record_count = LOG_MESSAGE_MAX.div_ceil(characters_per_record);
        let records = wait_for_records(record_count);
        let joined: String = records
            .iter()
            .map(|record| record.message.as_str())
            .collect();
        let expected = "\\u{1b}".repeat(LOG_MESSAGE_MAX);
        kernel_tests::kassert!(joined == expected);
        kernel_tests::kassert!(
            records
                .iter()
                .all(|record| record.message.len() <= LOG_MESSAGE_MAX)
        );
    });
}

// Обычный печатный UTF-8 relay передаёт без изменений.
#[kernel_test]
fn relay_keeps_ordinary_utf8_unchanged() {
    with_relay(|session| {
        let logger = session(NAMED_BADGE);
        write_log(&logger, "播放器", "こんにちは, 世界!");

        let records = wait_for_records(1);
        kernel_tests::kassert!(records[0].source == "named-subsystem/播放器");
        kernel_tests::kassert!(records[0].message == "こんにちは, 世界!");
    });
}

// Враждебные tag/message не дают стоку строку без trusted badge-prefix и не
// оставляют физических управляющих символов.
#[kernel_test]
fn relay_never_writes_an_unprefixed_physical_line() {
    with_relay(|session| {
        let logger = session(NAMED_BADGE);
        write_log(&logger, "]\n[forged", "ok\n\u{1b}[31mforged");

        let records = wait_for_records(1);
        kernel_tests::kassert!(records[0].source.starts_with("named-subsystem/"));
        kernel_tests::kassert!(!records[0].source.chars().any(char::is_control));
        kernel_tests::kassert!(!records[0].message.chars().any(char::is_control));
    });
}

// Пустой message сохраняет семантику одного входного кадра и одной записи.
#[kernel_test]
fn relay_forwards_an_empty_message_once() {
    with_relay(|session| {
        let logger = session(NAMED_BADGE);
        write_log(&logger, "local", "");

        let records = wait_for_records(1);
        kernel_tests::kassert_eq!(records.len(), 1);
        kernel_tests::kassert!(records[0].message.is_empty());
    });
}

// Уход сессии relay не останавливает: следующая сессия обслуживается.
#[kernel_test]
fn relay_serves_past_a_departed_session() {
    with_relay(|session| {
        let departing = session(UNNAMED_BADGE);
        write_log(&departing, "first", "before departure");
        wait_for_records(1);
        drop(departing);

        let logger = session(NAMED_BADGE);
        write_log(&logger, "second", "after departure");
        let records = wait_for_records(2);
        kernel_tests::kassert!(records[1].message == "after departure");
    });
}

// Кадр вне контракта отвергается по одному: следующий валидный кадр relay
// пересылает.
#[kernel_test]
fn relay_rejects_one_bad_frame_and_serves_the_next() {
    with_relay(|session| {
        let logger = session(NAMED_BADGE);
        send_unknown_ordinal(&logger);
        write_log(&logger, "local", "after a bad frame");

        let records = wait_for_records(1);
        kernel_tests::kassert!(records[0].message == "after a bad frame");
    });
}

// Capability, приложенная к кадру лога, дальше стока не идёт: relay закрывает
// её, и порт остаётся без получателей.
#[kernel_test]
fn relay_closes_capabilities_carried_by_a_log_frame() {
    with_relay(|session| {
        let logger = session(NAMED_BADGE);
        let carried = Port::create().expect("carried port");
        let writer = carried
            .handle()
            .duplicate(Rights::WRITE | Rights::TRANSFER, None)
            .expect("writer side");
        // Корневая капа - единственный получатель: она уезжает кадром.
        kernel_tests::kassert_eq!(send_log_frame_with_cap(&logger, carried.into_handle()), 0);
        wait_for_records(1);

        // Закрытие корневой капы отзывает выведенную из неё клиентскую, а
        // сохранённая капа оставляет отправителя ждать до срока.
        kernel_tests::kassert_eq!(
            port_send(writer.as_raw(), SEND_TIMEOUT),
            SyscallError::Revoked.as_return_value()
        );
    });
}

// Нехватка слота под capability отклоняет один кадр на стороне relay. По
// освободившимся слотам следующий обычный кадр доходит до стока.
#[kernel_test]
fn relay_serves_after_receiver_resource_exhaustion() {
    with_relay(|session| {
        let logger = session(NAMED_BADGE);
        let (_source, mut fillers) = exhaust_handle_table();
        let carried = fillers.pop().expect("full table contains filler handles");

        kernel_tests::kassert_eq!(
            send_log_frame_with_cap(&logger, carried),
            SyscallError::OutOfHandles.as_return_value()
        );
        // После проверенного отказа свободный слот один, а претендента два: транзитная
        // капа стока в потоке relay и порт-таймер ожидания записей. Освобождение ещё
        // двух слотов снимает гонку потоков за единственный, не трогая сам сценарий.
        for _ in 0..2 {
            drop(fillers.pop().expect("full table contains filler handles"));
        }
        write_log(&logger, "local", "after resource exhaustion");

        let records = wait_for_records(1);
        kernel_tests::kassert!(records[0].message == "after resource exhaustion");
    });
}

// Лог-капа не даёт полномочий bootstrap-контракта, и попытки их взять relay
// переживает.
#[kernel_test]
fn log_session_denies_bootstrap_operations() {
    with_relay(|session| {
        let fixture = spawn_image_entry(LOG_CLIENT_FIXTURE, session(NAMED_BADGE));
        fixture.join(JOIN_TIMEOUT).expect("fixture terminates");
        kernel_tests::kassert_eq!(fixture.exit_code().expect("exit code"), 0);

        // Строка фикстуры прошла тем же кадром, каким не прошли acquire-вызовы.
        let records = wait_for_records(1);
        kernel_tests::kassert!(records[0].message == FIXTURE_LINE);

        // Relay продолжает обслуживать порт после отвергнутых кадров.
        let next = session(NAMED_BADGE);
        write_log(&next, "local", "relay is alive");
        let records = wait_for_records(2);
        kernel_tests::kassert!(records[1].message == "relay is alive");
    });
}

// Уходящие сессии не копят непрочтённых событий: минт новых продолжает
// проходить и после смены их числа, кратного ёмкости таблицы.
#[kernel_test]
fn relay_drains_departed_badges_so_new_sessions_mint() {
    with_relay(|session| {
        let mut minted = 0;
        let mut records = 0;
        while minted < u64::from(HANDLE_TABLE_CAPACITY) + CHURN_BATCH {
            for badge in minted..minted + CHURN_BATCH {
                drop(session(NonZeroU64::MIN.saturating_add(badge)));
            }
            minted += CHURN_BATCH;
            // Доставленный кадр означает, что relay разобрал накопленные уходы.
            // База синхронизирующих значков не пересекается с меняющимися.
            let sync = session(SYNC_BADGE_BASE.saturating_add(minted));
            write_log(&sync, "churn", "session after churn");
            records += 1;
            wait_for_records(records);
        }
    });
}

// Закрытие лог-порта владельцем завершает цикл relay штатно: сторона приёма
// отзывается каскадом от корневой капы.
#[kernel_test]
fn relay_stops_when_its_log_port_closes() {
    let relay =
        LogRelayThread::start(TestSourceNames, RecordingSink).expect("test log relay starts");
    relay.shutdown(JOIN_TIMEOUT).expect("test log relay stops");
}

// Первый отказ стока прекращает куски текущего кадра. Сам кадр учитывается
// одним отказом, и следующий успешный кадр сбрасывает последовательность.
#[kernel_test]
fn relay_stops_a_frame_at_the_first_refused_chunk() {
    with_relay(|session| {
        let logger = session(NAMED_BADGE);
        let message = "\u{1b}".repeat(LOG_MESSAGE_MAX);

        SINK_REFUSES.store(true, SeqCst);
        write_log(&logger, "local", &message);
        let records = wait_for_records(1);
        kernel_tests::kassert_eq!(records.len(), 1);

        SINK_REFUSES.store(false, SeqCst);
        write_log(&logger, "local", "accepted after refused frame");
        let records = wait_for_records(2);
        kernel_tests::kassert!(records[1].message == "accepted after refused frame");
    });
}

// Предел подряд идущих отказов стока уводит relay из цикла, а прерванная
// успехом череда отказов его не трогает.
#[kernel_test]
fn relay_stops_after_the_sink_refuses_in_a_row() {
    with_relay(|session| {
        let logger = session(NAMED_BADGE);

        SINK_REFUSES.store(true, SeqCst);
        for _ in 1..SINK_FAILURE_LIMIT {
            write_log(&logger, "local", "refused");
        }
        SINK_REFUSES.store(false, SeqCst);
        write_log(&logger, "local", "accepted");
        let records = wait_for_records(SINK_FAILURE_LIMIT);
        kernel_tests::kassert!(records[SINK_FAILURE_LIMIT - 1].message == "accepted");

        SINK_REFUSES.store(true, SeqCst);
        for _ in 0..SINK_FAILURE_LIMIT {
            write_log(&logger, "local", "refused");
        }
        wait_for_records(2 * SINK_FAILURE_LIMIT);
        SINK_REFUSES.store(false, SeqCst);

        kernel_tests::kassert_eq!(
            port_send(logger.as_raw(), SEND_TIMEOUT),
            SyscallError::Timeout.as_return_value()
        );
    });
}
