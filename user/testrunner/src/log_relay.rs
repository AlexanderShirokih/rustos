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
use core::sync::atomic::{AtomicBool, Ordering::SeqCst};

use bootstrap::BootstrapClient;
use ipc::wire::{Header, IpcError, MESSAGE_INLINE_MAX, MessageBuf};
use kernel_tests::kernel_test;
use log_abi::{LogClient, log_ordinal};
use log_relay::{KernelLogSink, LogRelayThread, LogSink};
use runtime::{
    MemoryRegion, OwnedHandle, Port, PortTransport, Process, Resource, Timeout, port_send,
};
use spin::Mutex;
use syscall::{HANDLE_TABLE_CAPACITY, Rights, SyscallError};
use userland_image::decode;
use userland_loader::load_entry;

use crate::{ipc_raw, log_fixture_protocol::FIXTURE_LINE};

// Значок лог-сессии маршрутизатора имён у общего relay прогона.
pub(crate) const ROUTER_BADGE: u64 = 0x1001_1001_1001_1001;

// Значки тестовых сессий: первый назван таблицей, второй в неё не входит.
const NAMED_BADGE: u64 = 0x0101_0101_0101_0101;
const UNNAMED_BADGE: u64 = 0x0202_0202_0202_0202;

fn shared_source_name(badge: u64) -> Option<&'static str> {
    (badge == ROUTER_BADGE).then_some("filesystem")
}

fn test_source_name(badge: u64) -> Option<&'static str> {
    (badge == NAMED_BADGE).then_some("named-subsystem")
}

// Фикстура, пробующая bootstrap-операции по выданной ей лог-капе.
const LOG_CLIENT_FIXTURE: &str = "/boot/log-client-fixture";

// Источник собственных строк relay: диагностика отвергнутых кадров идёт в тот
// же сток, что и клиентские строки.
const RELAY_SOURCE: &str = "log-relay";

// Срок ожидания записи стока: пересылка идёт в потоке relay.
const RECORD_TIMEOUT_NS: u64 = 3_000_000_000;
// Шаг ожидания записи стока.
const RECORD_POLL_NS: u64 = 10_000_000;
// Срок остановки relay и завершения фикстуры.
const JOIN_TIMEOUT_NS: u64 = 5_000_000_000;
// Срок отправки, за который живой получатель успевает встретить кадр.
const SEND_TIMEOUT_NS: u64 = 500_000_000;

// Подряд идущих отказов стока, после которых цикл relay уходит. Значение
// повторяет предел `lib/log-relay`: тест проверяет его границу с обеих сторон.
const SINK_FAILURE_LIMIT: usize = 8;

// Сессий, минтящихся между синхронизациями с relay: непрочтённых событий об
// уходе значка порт держит не больше ёмкости таблицы хэндлов.
const CHURN_BATCH: u64 = 64;

// База значков синхронизирующих сессий: с меняющимися значками она не
// пересекается.
const SYNC_BADGE_BASE: u64 = 0x0300_0000;

// Строка, пересланная стоком тестового relay.
#[derive(Clone)]
struct Record {
    source: String,
    message: String,
}

static RECORDED: Mutex<Vec<Record>> = Mutex::new(Vec::new());

// Общий relay прогона: его сессии получают процессы, поднимаемые тестами.
static SHARED: Mutex<Option<LogRelayThread>> = Mutex::new(None);

// Relay идущего сценария: отказ kassert уходит из его тела прямо в путь
// завершения прогона, и остановить цикл может только он.
static ACTIVE: Mutex<Option<LogRelayThread>> = Mutex::new(None);

// Режим стока: отказ вместо пересылки.
static SINK_REFUSES: AtomicBool = AtomicBool::new(false);

// Лог-сессия маршрутизатора имён с правом минта: производные достаются его
// файловым системам.
pub(crate) fn router_log_session() -> OwnedHandle {
    let mut shared = SHARED.lock();
    let relay = shared.get_or_insert_with(|| {
        LogRelayThread::start(
            shared_source_name,
            KernelLogSink::new(crate::bootstrap_handle()),
        )
        .expect("shared log relay starts")
    });
    relay
        .delegated_session(ROUTER_BADGE)
        .expect("router log session")
}

// Останавливает циклы relay прогона: их потоки иначе остаются живыми и не дают
// процессу завершиться. Путь завершения не ждёт лока: занятый лок оставляет
// цикл живым, а ожидание превращает провал теста в зависание.
pub(crate) fn shutdown_relays() {
    stop(&ACTIVE);
    stop(&SHARED);
}

fn stop(slot: &Mutex<Option<LogRelayThread>>) {
    if let Some(mut held) = slot.try_lock()
        && let Some(relay) = held.take()
    {
        let _ = relay.shutdown(Timeout::from_ns(JOIN_TIMEOUT_NS));
    }
}

// Сток тестового relay.
struct RecordingSink;

impl LogSink for RecordingSink {
    fn write(&mut self, source: &str, message: &str) -> Result<(), IpcError> {
        let kernel = BootstrapClient::new(PortTransport::client(crate::bootstrap_handle()));
        let forwarded = kernel.log_str(source, message);
        // Двусторонний вызов ядра перезаписывает значок в IPC-буфере потока.
        if let Ok(Ok(cap)) = kernel.acquire_userland_image() {
            drop(OwnedHandle::adopt(cap));
        }
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

// Минт лог-сессии идущего сценария. Лок держится только на время минта, и путь
// завершения прогона застаёт его свободным.
fn scenario_session(badge: u64) -> OwnedHandle {
    ACTIVE
        .lock()
        .as_ref()
        .expect("scenario relay is running")
        .session(badge)
        .expect("log session")
}

// Поднимает тестовый relay, отдаёт сценарию минт его сессий и останавливает.
fn with_relay(body: impl FnOnce(&dyn Fn(u64) -> OwnedHandle)) {
    RECORDED.lock().clear();
    SINK_REFUSES.store(false, SeqCst);
    *ACTIVE.lock() = Some(
        LogRelayThread::start(test_source_name, RecordingSink).expect("test log relay starts"),
    );

    body(&scenario_session);

    SINK_REFUSES.store(false, SeqCst);
    ACTIVE
        .lock()
        .take()
        .expect("scenario relay is running")
        .shutdown(Timeout::from_ns(JOIN_TIMEOUT_NS))
        .expect("test log relay stops");
}

// Ждёт, пока сток снимет `count` клиентских записей, и отдаёт их снимок.
// Собственные строки relay в счёт не идут.
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
        kernel_tests::kassert!(waited < RECORD_TIMEOUT_NS);
        let _ = idle.recv(Timeout::from_ns(RECORD_POLL_NS));
        waited += RECORD_POLL_NS;
    }
}

// Пишет строку по лог-сессии.
fn write_log(session: &OwnedHandle, tag: &str, message: &str) {
    LogClient::new(PortTransport::client(session))
        .write_str(tag, message)
        .expect("log frame delivered");
}

// Шлёт по лог-сессии кадр с ordinal вне контракта.
fn send_unknown_ordinal(session: &OwnedHandle) {
    let mut frame = MessageBuf::<{ MESSAGE_INLINE_MAX }>::new();
    frame
        .write_header(&Header::new(log_ordinal::WRITE ^ 1, 0, 0))
        .expect("header fits");
    frame.finish().expect("frame fits");
    ipc_raw::write_frame(frame.as_bytes(), &[]);
    kernel_tests::kassert_eq!(
        port_send(session.as_raw(), Timeout::from_ns(SEND_TIMEOUT_NS).raw()),
        0
    );
}

// Шлёт по лог-сессии валидный кадр лога с приложенной capability.
fn send_log_frame_with_cap(session: &OwnedHandle, cap: OwnedHandle) {
    let mut frame = MessageBuf::<{ MESSAGE_INLINE_MAX }>::new();
    frame
        .write_header(&Header::new(log_ordinal::WRITE, 0, 0))
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
    let sent = port_send(session.as_raw(), Timeout::from_ns(SEND_TIMEOUT_NS).raw());
    kernel_tests::kassert_eq!(sent, 0);
}

// Спавнит программу образа, передавая ей `start` стартовым хэндлом.
fn spawn_image_entry(path: &str, start: OwnedHandle) -> Process {
    let kernel = BootstrapClient::new(PortTransport::client(crate::bootstrap_handle()));
    let cap = kernel
        .acquire_userland_image()
        .expect("acquire_userland_image call")
        .expect("image vend");
    let region = MemoryRegion::from_handle(OwnedHandle::adopt(cap));
    let mapping = region.map_full_read_only().expect("map image read-only");
    let image = decode(mapping.as_bytes()).expect("decode userland image");
    let program = image
        .entry(path)
        .expect("image carries the entry")
        .program()
        .expect("entry record carries a program");
    let resource = Resource::self_resource().expect("self metering resource");
    load_entry(&program, &resource, start).expect("spawn from image")
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
            .duplicate(Rights::WRITE | Rights::TRANSFER, 0)
            .expect("writer side");
        // Корневая капа - единственный получатель: она уезжает кадром.
        send_log_frame_with_cap(&logger, carried.into_handle());
        wait_for_records(1);

        // Закрытие корневой капы отзывает выведенную из неё клиентскую, а
        // сохранённая капа оставляет отправителя ждать до срока.
        kernel_tests::kassert_eq!(
            port_send(writer.as_raw(), Timeout::from_ns(SEND_TIMEOUT_NS).raw()),
            SyscallError::Revoked.as_return_value()
        );
    });
}

// Лог-капа не даёт полномочий bootstrap-контракта, и попытки их взять relay
// переживает.
#[kernel_test]
fn log_session_denies_bootstrap_operations() {
    with_relay(|session| {
        let fixture = spawn_image_entry(LOG_CLIENT_FIXTURE, session(NAMED_BADGE));
        fixture
            .join(Timeout::from_ns(JOIN_TIMEOUT_NS))
            .expect("fixture terminates");
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
                drop(session(badge + 1));
            }
            minted += CHURN_BATCH;
            // Доставленный кадр означает, что relay разобрал накопленные уходы.
            let sync = session(SYNC_BADGE_BASE + minted);
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
        LogRelayThread::start(test_source_name, RecordingSink).expect("test log relay starts");
    relay
        .shutdown(Timeout::from_ns(JOIN_TIMEOUT_NS))
        .expect("test log relay stops");
}

// Предел подряд идущих отказов стока уводит relay из цикла, а прерванная
// успехом череда отказов его не трогает. Оставшийся без цикла порт держит
// получателя корневой капой владельца, и кадру некому встретиться.
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
            port_send(logger.as_raw(), Timeout::from_ns(SEND_TIMEOUT_NS).raw()),
            SyscallError::Timeout.as_return_value()
        );
    });
}
