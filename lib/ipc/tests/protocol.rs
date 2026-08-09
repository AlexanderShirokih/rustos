//! Интеграция кодогена `protocol!`: round-trip client -> dispatch -> service
//! через блокирующий host-mock транспорта на двух потоках. Покрывает успех,
//! доменную ошибку (DOMAIN_ERR), неизвестный ordinal (PEER_CLOSE), cast, event
//! и txid-корреляцию (R2).

use std::{
    cell::RefCell,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

use ipc::{
    Transport,
    wire::{Bytes, Header, Str},
};
use ipc_test::MockEnd;

const ERR_DIVIDE_BY_ZERO: u32 = 1;

ipc::protocol! {
    // Контракт со всеми видами операций сразу.
    mod calc {
        name = "Calc";

        call fn add(a: u32, b: u32) -> u32;
        call fn div(a: u32, b: u32) -> Result<u32, u32>;
        call fn echo_len(text: Str<16>) -> u32;
        cast fn reset(seed: u64);
        event fn tick(seq: u64);
    }
}

#[derive(Default)]
struct CalcServer {
    seed: u64,
}

impl calc::Service for CalcServer {
    fn add(&mut self, a: u32, b: u32) -> u32 {
        a + b
    }

    fn div(&mut self, a: u32, b: u32) -> Result<u32, u32> {
        a.checked_div(b).ok_or(ERR_DIVIDE_BY_ZERO)
    }

    fn echo_len(&mut self, text: Str<16>) -> u32 {
        text.as_str().len() as u32
    }

    fn reset(&mut self, seed: u64) {
        self.seed = seed;
    }
}

fn round_trip<R: Send>(
    rounds: usize,
    client_body: impl FnOnce(&calc::Client<MockEnd>) -> R + Send,
) -> (R, CalcServer) {
    let (client_end, server_end) = MockEnd::pair();
    let client = calc::Client::new(client_end);
    thread::scope(|scope| {
        let server_handle = scope.spawn(move || {
            let mut server = CalcServer::default();
            for _ in 0..rounds {
                server_end
                    .wait_readable(ipc::Timeout::INFINITE)
                    .expect("server wait");
                calc::dispatch(&mut server, &server_end).expect("dispatch ok");
            }
            server
        });
        let result = client_body(&client);
        let server = server_handle.join().expect("server thread");
        (result, server)
    })
}

#[test]
fn two_way_without_error_round_trip() {
    let (result, _server) = round_trip(1, |client| client.add(7, 35));
    assert_eq!(result, Ok(42));
}

#[test]
fn two_way_domain_success() {
    let (result, _server) = round_trip(1, |client| client.div(84, 2));
    assert_eq!(result, Ok(Ok(42)));
}

#[test]
fn two_way_domain_error_keeps_channel() {
    // Деление на ноль -> доменная ошибка (DOMAIN_ERR), затем канал ещё работает.
    let (result, _server) = round_trip(2, |client| {
        let err = client.div(1, 0);
        let ok = client.add(20, 22);
        (err, ok)
    });
    assert_eq!(result.0, Ok(Err(ERR_DIVIDE_BY_ZERO)));
    assert_eq!(result.1, Ok(42));
}

#[test]
fn two_way_bounded_str_param() {
    let (result, _server) = round_trip(1, |client| {
        client.echo_len(Str::<16>::new("hello").expect("within bound"))
    });
    assert_eq!(result, Ok(5));
}

#[test]
fn cast_delivers_without_response() {
    let (_unit, server) = round_trip(1, |client| {
        client.reset(0xDEAD_BEEF).expect("cast write ok");
    });
    assert_eq!(server.seed, 0xDEAD_BEEF);
}

#[test]
fn unknown_ordinal_yields_peer_close() {
    let (client_end, server_end) = MockEnd::pair();
    let mut server = CalcServer::default();

    // Кадр с чужим ordinal: dispatch отвечает PEER_CLOSE, не паникует (R3).
    let mut frame = ipc::wire::MessageBuf::<{ ipc::wire::MESSAGE_INLINE_MAX }>::new();
    frame
        .write_header(&Header::new(0xDEAD_DEAD_DEAD_DEAD, 9, 0))
        .expect("header ok");
    frame.finish().expect("finish ok");
    client_end
        .write_message(frame.as_bytes(), &[])
        .expect("write ok");

    calc::dispatch(&mut server, &server_end).expect("dispatch returns ok");

    let mut bytes = [0u8; 64];
    let mut handles = [0u32; 4];
    let len = client_end
        .read_message(&mut bytes, &mut handles)
        .expect("peer-close frame present");
    let header = Header::decode(&bytes[..len.bytes]).expect("decode");
    assert!(header.has_flag(ipc::wire::FLAG_PEER_CLOSE));
    assert_eq!(header.txid, 9);
}

#[test]
fn close_frame_yields_peer_closed_from_dispatch() {
    let (client_end, server_end) = MockEnd::pair();
    let mut server = CalcServer::default();

    // Закрывающий кадр: dispatch возвращает PeerClosed, серверный цикл выходит.
    let mut frame = ipc::wire::MessageBuf::<{ ipc::wire::MESSAGE_INLINE_MAX }>::new();
    frame
        .write_header(&Header::new(0, 0, ipc::wire::FLAG_PEER_CLOSE))
        .expect("header ok");
    frame.finish().expect("finish ok");
    client_end
        .write_message(frame.as_bytes(), &[])
        .expect("write ok");

    assert_eq!(
        calc::dispatch(&mut server, &server_end),
        Err(ipc::wire::IpcError::PeerClosed)
    );
}

#[test]
fn mismatched_txid_is_skipped() {
    let (client_end, server_end) = MockEnd::pair();
    let client = calc::Client::new(client_end);

    // Кадр-ответ с чужим txid вброшен в очередь до настоящего ответа.
    let mut stray = ipc::wire::MessageBuf::<{ ipc::wire::MESSAGE_INLINE_MAX }>::new();
    stray
        .write_header(&Header::new(
            calc::add::ORDINAL,
            0xFFFF,
            ipc::wire::FLAG_RESPONSE,
        ))
        .expect("header ok");
    stray
        .write_field(1, &ipc::wire::value::encode_u32(999))
        .expect("field ok");
    stray.finish().expect("finish ok");
    server_end
        .write_message(stray.as_bytes(), &[])
        .expect("write stray ok");

    // Затем корректный ответ на txid=1 (первый txid клиента).
    let mut good = ipc::wire::MessageBuf::<{ ipc::wire::MESSAGE_INLINE_MAX }>::new();
    good.write_header(&Header::new(
        calc::add::ORDINAL,
        1,
        ipc::wire::FLAG_RESPONSE,
    ))
    .expect("header ok");
    good.write_field(1, &ipc::wire::value::encode_u32(42))
        .expect("field ok");
    good.finish().expect("finish ok");
    server_end
        .write_message(good.as_bytes(), &[])
        .expect("write good ok");

    // Клиент шлёт add (его запрос осядет в очереди сервера, тут не читается) и
    // читает ответ: кадр с чужим txid отброшен, принят кадр с txid=1 (R2).
    let result = client.add(0, 0);
    assert_eq!(result, Ok(42));
}

#[test]
fn event_round_trip() {
    let (server_end, client_end) = MockEnd::pair();

    // Сервер эмитит событие в клиентский конец.
    calc::EventSender::tick(&server_end, 0xABCD).expect("emit ok");

    let mut handler = TickHandler::default();
    calc::dispatch_event(&mut handler, &client_end).expect("dispatch event ok");
    assert_eq!(handler.last_seq, Some(0xABCD));
}

#[test]
fn descriptor_exposes_operations() {
    let desc = calc::DESC;
    assert_eq!(desc.operations.len(), 5);

    let add = desc
        .operations
        .iter()
        .find(|op| op.name == "add")
        .expect("add present");
    assert_eq!(add.ordinal, calc::add::ORDINAL);
    assert_eq!(add.ordinal, 0xb744_87cd_9b28_41ba);
    assert_eq!(add.canonical, "Calc.add");
    assert_eq!(add.kind, ipc::schema::Kind::Call);
    assert_eq!(add.fields.len(), 2);
    assert_eq!(add.fields[0].field_id, 1);
    assert_eq!(add.fields[0].field_type, ipc::schema::WireType::Uint(4));
    assert_eq!(add.fields[1].field_id, 2);

    let echo = desc
        .operations
        .iter()
        .find(|op| op.name == "echo_len")
        .expect("echo present");
    assert_eq!(
        echo.fields[0].field_type,
        ipc::schema::WireType::BoundedStr(16)
    );

    let reset = desc
        .operations
        .iter()
        .find(|op| op.name == "reset")
        .expect("reset present");
    assert_eq!(reset.kind, ipc::schema::Kind::Cast);

    let tick = desc
        .operations
        .iter()
        .find(|op| op.name == "tick")
        .expect("tick present");
    assert_eq!(tick.kind, ipc::schema::Kind::Event);
}

#[test]
fn ordinal_constants_match_golden() {
    assert_eq!(calc::add::ORDINAL, 0xb744_87cd_9b28_41ba);
    assert_eq!(calc::div::ORDINAL, 0x2420_7b95_a0d9_84fc);
    assert_eq!(calc::reset::ORDINAL, 0x1cb8_08a6_5161_cd40);
}

#[test]
fn bounded_bytes_param_round_trips() {
    let (blob_client_end, blob_server_end) = MockEnd::pair();
    let client = blob::Client::new(blob_client_end);
    let mut server = BlobServer::default();

    client
        .store(Bytes::<8>::new(&[1, 2, 3, 4]).expect("within bound"))
        .expect("cast write ok");
    blob::dispatch(&mut server, &blob_server_end).expect("dispatch ok");
    assert_eq!(server.last_len, 4);
}

ipc::protocol! {
    // Контракт с байтовым полем.
    mod blob {
        name = "Blob";

        cast fn store(data: Bytes<8>);
    }
}

ipc::protocol! {
    // Контракт поверх SPSC-кольца: только односторонние операции.
    mod counter {
        name = "Counter", plane = ring;

        cast fn sample(value: u64);
        event fn overflow(count: u32);
    }
}

#[derive(Default)]
struct CounterServer {
    last_sample: u64,
}

impl counter::Service for CounterServer {
    fn sample(&mut self, value: u64) {
        self.last_sample = value;
    }
}

#[derive(Default)]
struct OverflowHandler {
    last_count: Option<u32>,
}

impl counter::Events for OverflowHandler {
    fn overflow(&mut self, count: u32) {
        self.last_count = Some(count);
    }
}

#[test]
fn ring_plane_cast_round_trips() {
    let (client_end, server_end) = MockEnd::pair();
    let client = counter::Client::new(client_end);
    let mut server = CounterServer::default();

    client.sample(0xCAFE).expect("cast write ok");
    counter::dispatch(&mut server, &server_end).expect("dispatch ok");
    assert_eq!(server.last_sample, 0xCAFE);
}

#[test]
fn ring_plane_event_round_trips() {
    let (server_end, client_end) = MockEnd::pair();

    counter::EventSender::overflow(&server_end, 17).expect("emit ok");

    let mut handler = OverflowHandler::default();
    counter::dispatch_event(&mut handler, &client_end).expect("dispatch event ok");
    assert_eq!(handler.last_count, Some(17));
}

#[test]
fn transport_plane_const_reflects_attribute() {
    assert_eq!(counter::TRANSPORT_PLANE, "ring");
    // Дефолт (без transport) - port.
    assert_eq!(calc::TRANSPORT_PLANE, "port");
}

#[derive(Default)]
struct BlobServer {
    last_len: usize,
}

impl blob::Service for BlobServer {
    fn store(&mut self, data: Bytes<8>) {
        self.last_len = data.as_bytes().len();
    }
}

#[derive(Default)]
struct TickHandler {
    last_seq: Option<u64>,
}

impl calc::Events for TickHandler {
    fn tick(&mut self, seq: u64) {
        self.last_seq = Some(seq);
    }
}

ipc::protocol! {
    // Контракт передачи capability в обе стороны.
    mod cap_proto {
        name = "CapProto";

        call fn echo_cap(c: Cap) -> Cap;
        call fn acquire(ok: bool) -> Result<ipc::wire::Cap, u32>;
    }
}

ipc::protocol! {
    // Контракт односторонней передачи владения capability.
    mod owned_cast {
        name = "OwnedCast";

        cast fn deliver(capability: Cap);
    }
}

struct CastOutcome {
    result: Result<(), ipc::wire::IpcError>,
    handles: RefCell<Vec<u32>>,
}

impl CastOutcome {
    fn new(result: Result<(), ipc::wire::IpcError>) -> Self {
        Self {
            result,
            handles: RefCell::new(Vec::new()),
        }
    }
}

impl Transport for CastOutcome {
    fn write_message(&self, _bytes: &[u8], handles: &[u32]) -> Result<(), ipc::wire::IpcError> {
        self.handles.borrow_mut().extend_from_slice(handles);
        self.result
    }

    fn read_message(
        &self,
        _bytes: &mut [u8],
        _handles: &mut [u32],
    ) -> Result<ipc::MessageLen, ipc::wire::IpcError> {
        Err(ipc::wire::IpcError::WouldBlock)
    }

    fn wait_readable(&self, _timeout: ipc::Timeout) -> Result<(), ipc::wire::IpcError> {
        Err(ipc::wire::IpcError::WouldBlock)
    }
}

struct TrackedHandle {
    raw: u32,
    drops: Arc<AtomicUsize>,
    relinquishes: Arc<AtomicUsize>,
}

impl Drop for TrackedHandle {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

impl ipc::IntoWireHandle for TrackedHandle {
    fn wire_id(&self) -> u32 {
        self.raw
    }

    fn relinquish(self) {
        self.relinquishes.fetch_add(1, Ordering::SeqCst);
        core::mem::forget(self);
    }
}

#[test]
fn successful_cast_relinquishes_owned_handle() {
    let drops = Arc::new(AtomicUsize::new(0));
    let relinquishes = Arc::new(AtomicUsize::new(0));
    let transport = CastOutcome::new(Ok(()));
    let client = owned_cast::Client::new(transport);

    client
        .deliver(TrackedHandle {
            raw: 41,
            drops: Arc::clone(&drops),
            relinquishes: Arc::clone(&relinquishes),
        })
        .expect("cast succeeds");

    assert_eq!(client.into_transport().handles.into_inner(), vec![41]);
    assert_eq!(relinquishes.load(Ordering::SeqCst), 1);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
}

#[test]
fn timed_out_cast_drops_owned_handle_without_relinquishing() {
    let drops = Arc::new(AtomicUsize::new(0));
    let relinquishes = Arc::new(AtomicUsize::new(0));
    let client = owned_cast::Client::new(CastOutcome::new(Err(ipc::wire::IpcError::Timeout)));

    let result = client.deliver(TrackedHandle {
        raw: 42,
        drops: Arc::clone(&drops),
        relinquishes: Arc::clone(&relinquishes),
    });

    assert_eq!(result, Err(ipc::wire::IpcError::Timeout));
    assert_eq!(relinquishes.load(Ordering::SeqCst), 0);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

struct CapServer;

impl cap_proto::Service for CapServer {
    fn echo_cap(&mut self, c: ipc::wire::Cap) -> impl ipc::IntoWireHandle {
        c
    }

    fn acquire(&mut self, ok: bool) -> Result<ipc::wire::Cap, u32> {
        if ok {
            Ok(ipc::wire::Cap::from_raw(
                std::num::NonZeroU32::new(0x55).expect("non-zero"),
            ))
        } else {
            Err(7)
        }
    }
}

#[test]
fn cap_param_and_return_round_trip() {
    let (client_end, server_end) = MockEnd::pair();
    let client = cap_proto::Client::new(client_end);
    thread::scope(|scope| {
        let server = scope.spawn(move || {
            let mut srv = CapServer;
            for _ in 0..2 {
                server_end
                    .wait_readable(ipc::Timeout::INFINITE)
                    .expect("server wait");
                cap_proto::dispatch(&mut srv, &server_end).expect("dispatch ok");
            }
        });
        let sent = ipc::wire::Cap::from_raw(std::num::NonZeroU32::new(0x1234).expect("non-zero"));
        let echoed = client.echo_cap(sent).expect("echo ok");
        assert_eq!(echoed.raw().get(), 0x1234);

        let acquired = client.acquire(true).expect("acquire ok");
        assert_eq!(
            acquired,
            Ok(ipc::wire::Cap::from_raw(
                std::num::NonZeroU32::new(0x55).expect("non-zero")
            ))
        );
        server.join().expect("server thread");
    });
}

#[test]
fn cap_domain_error_carries_no_handle() {
    let (client_end, server_end) = MockEnd::pair();
    let client = cap_proto::Client::new(client_end);
    thread::scope(|scope| {
        let server = scope.spawn(move || {
            let mut srv = CapServer;
            server_end
                .wait_readable(ipc::Timeout::INFINITE)
                .expect("server wait");
            cap_proto::dispatch(&mut srv, &server_end).expect("dispatch ok");
        });
        let acquired = client.acquire(false).expect("acquire ok");
        assert_eq!(acquired, Err(7));
        server.join().expect("server thread");
    });
}

ipc::wire_value! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Point {
        x: u32,
        y: u32,
    }
}

ipc::wire_value! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Segment {
        from: Point,
        to: Point,
    }
}

ipc::wire_value! {
    struct Tagged<'a> {
        label: Str<'a, 8>,
        value: u32,
    }
}

ipc::wire_value! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Vended {
        session: ipc::wire::Cap,
        prefix_len: u32,
    }
}

ipc::protocol! {
    // Контракт с агрегатом, несущим capability.
    mod vendor {
        name = "Vendor";

        call fn vend(prefix_len: u32) -> Vended;
    }
}

struct VendorServer;

impl vendor::Service for VendorServer {
    fn vend(&mut self, prefix_len: u32) -> Vended {
        Vended {
            session: ipc::wire::Cap::from_raw(std::num::NonZeroU32::new(0x5151).expect("non-zero")),
            prefix_len,
        }
    }
}

#[test]
fn aggregate_carries_a_capability_alongside_scalars() {
    let (client_end, server_end) = MockEnd::pair();
    let client = vendor::Client::new(client_end);
    thread::scope(|scope| {
        let server = scope.spawn(move || {
            server_end
                .wait_readable(ipc::Timeout::INFINITE)
                .expect("server wait");
            vendor::dispatch(&mut VendorServer, &server_end).expect("dispatch ok");
        });

        let vended = client.vend(7).expect("vend ok");
        assert_eq!(vended.prefix_len, 7);
        assert_eq!(vended.session.raw().get(), 0x5151);

        server.join().expect("server joins");
    });
}

ipc::protocol! {
    // Контракт с агрегатами, включая вложенный и заимствующий.
    mod geo {
        name = "Geo";

        call fn translate(p: Point, dx: u32, dy: u32) -> Point;
        call fn flip(s: Segment) -> Segment;
        call fn label_len(t: Tagged) -> u32;
    }
}

struct GeoServer;

impl geo::Service for GeoServer {
    fn translate(&mut self, p: Point, dx: u32, dy: u32) -> Point {
        Point {
            x: p.x + dx,
            y: p.y + dy,
        }
    }

    fn flip(&mut self, s: Segment) -> Segment {
        Segment {
            from: s.to,
            to: s.from,
        }
    }

    fn label_len(&mut self, t: Tagged<'_>) -> u32 {
        t.label.as_str().len() as u32 + t.value
    }
}

#[test]
fn aggregate_param_and_return_round_trip() {
    let (client_end, server_end) = MockEnd::pair();
    let client = geo::Client::new(client_end);
    thread::scope(|scope| {
        let server = scope.spawn(move || {
            let mut srv = GeoServer;
            for _ in 0..3 {
                server_end
                    .wait_readable(ipc::Timeout::INFINITE)
                    .expect("server wait");
                geo::dispatch(&mut srv, &server_end).expect("dispatch ok");
            }
        });
        let moved = client
            .translate(Point { x: 10, y: 20 }, 3, 4)
            .expect("translate ok");
        assert_eq!(moved, Point { x: 13, y: 24 });

        // Вложенный агрегат: рекурсивный суб-кадр в параметре и возврате.
        let flipped = client
            .flip(Segment {
                from: Point { x: 1, y: 2 },
                to: Point { x: 3, y: 4 },
            })
            .expect("flip ok");
        assert_eq!(
            flipped,
            Segment {
                from: Point { x: 3, y: 4 },
                to: Point { x: 1, y: 2 },
            }
        );

        let tagged = Tagged {
            label: Str::<8>::new("ab").expect("within bound"),
            value: 40,
        };
        let len = client.label_len(tagged).expect("label_len ok");
        assert_eq!(len, 42);

        server.join().expect("server thread");
    });
}

#[test]
fn aggregate_descriptor_is_structural() {
    const POINT_DESC: ipc::schema::WireType = ipc::schema::WireType::Aggregate(&[
        ipc::schema::WireType::Uint(4),
        ipc::schema::WireType::Uint(4),
    ]);
    const SEGMENT_DESC: ipc::schema::WireType =
        ipc::schema::WireType::Aggregate(&[POINT_DESC, POINT_DESC]);

    let translate = geo::DESC
        .operations
        .iter()
        .find(|op| op.name == "translate")
        .expect("translate present");
    assert_eq!(
        translate.fields[0].field_type,
        ipc::schema::WireType::Aggregate(&[
            ipc::schema::WireType::Uint(4),
            ipc::schema::WireType::Uint(4),
        ])
    );

    let flip = geo::DESC
        .operations
        .iter()
        .find(|op| op.name == "flip")
        .expect("flip present");
    assert_eq!(flip.fields[0].field_type, SEGMENT_DESC);

    let label_len = geo::DESC
        .operations
        .iter()
        .find(|op| op.name == "label_len")
        .expect("label_len present");
    assert_eq!(
        label_len.fields[0].field_type,
        ipc::schema::WireType::Aggregate(&[
            ipc::schema::WireType::BoundedStr(8),
            ipc::schema::WireType::Uint(4),
        ])
    );
}

ipc::wire_value! {
    struct Oversized<'a> {
        blob: ipc::wire::FieldBytes<'a>,
    }
}

#[test]
fn aggregate_subframe_overflow_is_error() {
    // Суб-кадр сверх FIELD_DATA_MAX -> IpcError, без паники.
    let storage = [7u8; ipc::wire::FIELD_DATA_MAX];
    let big = Oversized {
        blob: ipc::wire::FieldBytes::new(&storage).expect("within bound"),
    };
    let mut buf = ipc::wire::MessageBuf::<{ ipc::wire::MESSAGE_INLINE_MAX }>::new();
    assert_eq!(
        ipc::WireValue::write_as_field(&big, &mut buf, 1, &mut ipc::wire::HandleSlots::new()),
        Err(IpcError::FrameOverflow)
    );
}

ipc::protocol! {
    // Контракт со строкой во всю запись поля.
    mod wide {
        name = "Wide";

        cast fn note(text: ipc::wire::FieldStr);
    }
}

#[test]
fn field_str_bound_tracks_field_data_max() {
    let note = wide::DESC
        .operations
        .iter()
        .find(|op| op.name == "note")
        .expect("note present");
    assert_eq!(
        note.fields[0].field_type,
        ipc::schema::WireType::BoundedStr(ipc::wire::FIELD_DATA_MAX)
    );
}

use ipc::wire::IpcError;

ipc::protocol! {
    // Контракт с нулевым сроком ожидания ответа по умолчанию.
    mod timed {
        name = "Timed", wait = 0;

        call fn ping() -> u32;
    }
}

ipc::protocol! {
    // Контракт, где срок ожидания задан на отдельной операции.
    mod mixed {
        name = "Mixed";

        call(wait = 0) fn quick() -> u32;
        call fn patient() -> u32;
    }
}

#[test]
fn protocol_level_default_timeout_times_out() {
    // server_end жив (sender_open), но не отвечает -> wait_readable(POLL) -> Timeout.
    let (client_end, _server_end) = MockEnd::pair();
    let client = timed::Client::new(client_end);
    assert_eq!(client.wait(), ipc::Timeout::POLL);
    assert_eq!(client.ping(), Err(IpcError::Timeout));
}

#[test]
fn per_call_timeout_override_times_out() {
    let (client_end, _server_end) = MockEnd::pair();
    let client = mixed::Client::new(client_end);
    // Дефолт клиента бессрочный, но `quick` несёт собственный timeout_ns = 0.
    assert_eq!(client.wait(), ipc::Timeout::INFINITE);
    assert_eq!(client.quick(), Err(IpcError::Timeout));
}

#[test]
fn with_wait_builder_overrides_default() {
    let (client_end, _server_end) = MockEnd::pair();
    let client = calc::Client::new(client_end).with_wait(ipc::Timeout::POLL);
    assert_eq!(client.wait(), ipc::Timeout::POLL);
    // Сервера нет -> two-way call истекает по тайм-ауту.
    assert_eq!(client.add(1, 2), Err(IpcError::Timeout));
}
