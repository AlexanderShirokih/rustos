//! Интеграция кодогена `#[protocol]`: round-trip client -> dispatch -> service
//! через блокирующий host-mock транспорта на двух потоках. Покрывает успех,
//! доменную ошибку (DOMAIN_ERR), неизвестный ordinal (PEER_CLOSE), cast, event
//! и txid-корреляцию (R2).

use std::thread;

use ipc::{
    Transport,
    wire::{Bytes, Header, Str},
};
use ipc_test::MockEnd;

/// Доменная ошибка тестового калькулятора (закодирована как u32).
const ERR_DIVIDE_BY_ZERO: u32 = 1;

/// Тестовый протокол: two-way с Result и без, bounded-Str-параметр, cast, event.
#[ipc::protocol(name = "Calc")]
trait Calc {
    #[call]
    fn add(&self, a: u32, b: u32) -> u32;

    #[call]
    fn div(&self, a: u32, b: u32) -> Result<u32, u32>;

    #[call]
    fn echo_len(&self, text: Str<16>) -> u32;

    #[cast]
    fn reset(&self, seed: u64);

    #[event]
    fn tick(seq: u64);
}

/// Сервис: суммирует, делит с доменной ошибкой, считает длину, хранит seed.
#[derive(Default)]
struct CalcServer {
    seed: u64,
}

impl CalcService for CalcServer {
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

/// Прогоняет `client_body` на потоке клиента, обслуживая `rounds` кадров
/// сервером на встречном конце; возвращает результат тела клиента.
fn round_trip<R: Send>(
    rounds: usize,
    client_body: impl FnOnce(&CalcClient<MockEnd>) -> R + Send,
) -> (R, CalcServer) {
    let (client_end, server_end) = MockEnd::pair();
    let client = CalcClient::new(client_end);
    thread::scope(|scope| {
        let server_handle = scope.spawn(move || {
            let mut server = CalcServer::default();
            for _ in 0..rounds {
                server_end.wait_readable(u64::MAX).expect("server wait");
                dispatch_calc(&mut server, &server_end).expect("dispatch ok");
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

    dispatch_calc(&mut server, &server_end).expect("dispatch returns ok");

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
        dispatch_calc(&mut server, &server_end),
        Err(ipc::wire::IpcError::PeerClosed)
    );
}

#[test]
fn mismatched_txid_is_skipped() {
    let (client_end, server_end) = MockEnd::pair();
    let client = CalcClient::new(client_end);

    // Кадр-ответ с чужим txid вброшен в очередь до настоящего ответа.
    let mut stray = ipc::wire::MessageBuf::<{ ipc::wire::MESSAGE_INLINE_MAX }>::new();
    stray
        .write_header(&Header::new(
            calc_ordinal::ADD,
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
    good.write_header(&Header::new(calc_ordinal::ADD, 1, ipc::wire::FLAG_RESPONSE))
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
    CalcEventSender::emit_tick(&server_end, 0xABCD).expect("emit ok");

    let mut handler = TickHandler::default();
    dispatch_calc_event(&mut handler, &client_end).expect("dispatch event ok");
    assert_eq!(handler.last_seq, Some(0xABCD));
}

#[test]
fn descriptor_exposes_operations() {
    let desc = calc_ordinal::DESC;
    assert_eq!(desc.operations.len(), 5);

    let add = desc
        .operations
        .iter()
        .find(|op| op.name == "add")
        .expect("add present");
    assert_eq!(add.ordinal, calc_ordinal::ADD);
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
    assert_eq!(calc_ordinal::ADD, 0xb744_87cd_9b28_41ba);
    assert_eq!(calc_ordinal::DIV, 0x2420_7b95_a0d9_84fc);
    assert_eq!(calc_ordinal::RESET, 0x1cb8_08a6_5161_cd40);
}

#[test]
fn bounded_bytes_param_round_trips() {
    let (blob_client_end, blob_server_end) = MockEnd::pair();
    let client = BlobClient::new(blob_client_end);
    let mut server = BlobServer::default();

    client
        .store(Bytes::<8>::new(&[1, 2, 3, 4]).expect("within bound"))
        .expect("cast write ok");
    dispatch_blob(&mut server, &blob_server_end).expect("dispatch ok");
    assert_eq!(server.last_len, 4);
}

/// Проверяет Bytes-параметр и независимый namespace ordinal.
#[ipc::protocol(name = "Blob")]
trait Blob {
    #[cast]
    fn store(&self, data: Bytes<8>);
}

/// Ring-плоскостной протокол.
#[ipc::protocol(name = "Counter", transport = "ring")]
trait Counter {
    #[cast]
    fn sample(&self, value: u64);

    #[event]
    fn overflow(count: u32);
}

#[derive(Default)]
struct CounterServer {
    last_sample: u64,
}

impl CounterService for CounterServer {
    fn sample(&mut self, value: u64) {
        self.last_sample = value;
    }
}

/// Клиентский обработчик события `overflow`.
#[derive(Default)]
struct OverflowHandler {
    last_count: Option<u32>,
}

impl CounterEvents for OverflowHandler {
    fn overflow(&mut self, count: u32) {
        self.last_count = Some(count);
    }
}

#[test]
fn ring_plane_cast_round_trips() {
    let (client_end, server_end) = MockEnd::pair();
    let client = CounterClient::new(client_end);
    let mut server = CounterServer::default();

    client.sample(0xCAFE).expect("cast write ok");
    dispatch_counter(&mut server, &server_end).expect("dispatch ok");
    assert_eq!(server.last_sample, 0xCAFE);
}

#[test]
fn ring_plane_event_round_trips() {
    let (server_end, client_end) = MockEnd::pair();

    CounterEventSender::emit_overflow(&server_end, 17).expect("emit ok");

    let mut handler = OverflowHandler::default();
    dispatch_counter_event(&mut handler, &client_end).expect("dispatch event ok");
    assert_eq!(handler.last_count, Some(17));
}

#[test]
fn transport_plane_const_reflects_attribute() {
    assert_eq!(counter_ordinal::TRANSPORT_PLANE, "ring");
    // Дефолт (без transport) - port.
    assert_eq!(calc_ordinal::TRANSPORT_PLANE, "port");
}

#[derive(Default)]
struct BlobServer {
    last_len: usize,
}

impl BlobService for BlobServer {
    fn store(&mut self, data: Bytes<8>) {
        self.last_len = data.as_bytes().len();
    }
}

/// Клиентский обработчик события `tick`.
#[derive(Default)]
struct TickHandler {
    last_seq: Option<u64>,
}

impl CalcEvents for TickHandler {
    fn tick(&mut self, seq: u64) {
        self.last_seq = Some(seq);
    }
}

/// Протокол с capability: проверяет, что handle едет вне тела.
#[ipc::protocol(name = "CapProto")]
trait CapProto {
    #[call]
    fn echo_cap(&self, c: ipc::wire::Cap) -> ipc::wire::Cap;

    #[call]
    fn acquire(&self, ok: bool) -> Result<ipc::wire::Cap, u32>;
}

/// Сервис: возвращает присланный handle и минтит фиксированный по запросу.
struct CapServer;

impl CapProtoService for CapServer {
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
    let client = CapProtoClient::new(client_end);
    thread::scope(|scope| {
        let server = scope.spawn(move || {
            let mut srv = CapServer;
            for _ in 0..2 {
                server_end.wait_readable(u64::MAX).expect("server wait");
                dispatch_cap_proto(&mut srv, &server_end).expect("dispatch ok");
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
    let client = CapProtoClient::new(client_end);
    thread::scope(|scope| {
        let server = scope.spawn(move || {
            let mut srv = CapServer;
            server_end.wait_readable(u64::MAX).expect("server wait");
            dispatch_cap_proto(&mut srv, &server_end).expect("dispatch ok");
        });
        let acquired = client.acquire(false).expect("acquire ok");
        assert_eq!(acquired, Err(7));
        server.join().expect("server thread");
    });
}

/// Owned-агрегат: годен и в параметре, и в возврате two-way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ipc::WireValue)]
struct Point {
    x: u32,
    y: u32,
}

/// Вложенный агрегат: поля-агрегаты дают рекурсивный суб-кадр.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ipc::WireValue)]
struct Segment {
    from: Point,
    to: Point,
}

/// Агрегат с borrow-полем: проверяет derive с одним лайфтаймом (как параметр).
#[derive(ipc::WireValue)]
struct Tagged<'a> {
    label: Str<'a, 8>,
    value: u32,
}

/// Протокол с пользовательскими агрегатами в сигнатурах.
#[ipc::protocol(name = "Geo")]
trait Geo {
    #[call]
    fn translate(&self, p: Point, dx: u32, dy: u32) -> Point;

    #[call]
    fn flip(&self, s: Segment) -> Segment;

    #[call]
    fn label_len(&self, t: Tagged<'_>) -> u32;
}

struct GeoServer;

impl GeoService for GeoServer {
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
    let client = GeoClient::new(client_end);
    thread::scope(|scope| {
        let server = scope.spawn(move || {
            let mut srv = GeoServer;
            for _ in 0..3 {
                server_end.wait_readable(u64::MAX).expect("server wait");
                dispatch_geo(&mut srv, &server_end).expect("dispatch ok");
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
    // Вложенный агрегат описан рекурсивно: агрегат из агрегатов.
    const POINT_DESC: ipc::schema::WireType = ipc::schema::WireType::Aggregate(&[
        ipc::schema::WireType::Uint(4),
        ipc::schema::WireType::Uint(4),
    ]);
    const SEGMENT_DESC: ipc::schema::WireType =
        ipc::schema::WireType::Aggregate(&[POINT_DESC, POINT_DESC]);

    let translate = geo_ordinal::DESC
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

    let flip = geo_ordinal::DESC
        .operations
        .iter()
        .find(|op| op.name == "flip")
        .expect("flip present");
    assert_eq!(flip.fields[0].field_type, SEGMENT_DESC);

    let label_len = geo_ordinal::DESC
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

/// Агрегат, чей суб-кадр заведомо превышает `FIELD_DATA_MAX`: тело поля занимает
/// весь потолок, а overhead записи выводит суб-кадр за границу.
#[derive(ipc::WireValue)]
struct Oversized<'a> {
    blob: ipc::wire::FieldBytes<'a>,
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
        ipc::WireValue::write_as_field(&big, &mut buf, 1),
        Err(IpcError::FrameOverflow)
    );
}

/// Протокол с полем максимального размера: фиксирует, что граница `FieldStr`,
/// запекаемая кодогеном, совпадает с рантайм-`FIELD_DATA_MAX`.
#[ipc::protocol(name = "Wide")]
trait Wide {
    #[cast]
    fn note(&self, text: ipc::wire::FieldStr<'_>);
}

#[test]
fn field_str_bound_tracks_field_data_max() {
    let note = wide_ordinal::DESC
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

/// Дефолтный тайм-аут клиента из атрибута протокола.
#[ipc::protocol(name = "Timed", timeout_ns = 0)]
trait Timed {
    #[call]
    fn ping(&self) -> u32;
}

/// Per-call тайм-аут перекрывает (бессрочный) дефолт клиента.
#[ipc::protocol(name = "Mixed")]
trait Mixed {
    #[call(timeout_ns = 0)]
    fn quick(&self) -> u32;

    #[call]
    fn patient(&self) -> u32;
}

#[test]
fn protocol_level_default_timeout_times_out() {
    // server_end жив (sender_open), но не отвечает -> wait_readable(0) -> Timeout.
    let (client_end, _server_end) = MockEnd::pair();
    let client = TimedClient::new(client_end);
    assert_eq!(client.wait_ns(), 0);
    assert_eq!(client.ping(), Err(IpcError::Timeout));
}

#[test]
fn per_call_timeout_override_times_out() {
    let (client_end, _server_end) = MockEnd::pair();
    let client = MixedClient::new(client_end);
    // Дефолт клиента бессрочный, но `quick` несёт собственный timeout_ns = 0.
    assert_eq!(client.wait_ns(), u64::MAX);
    assert_eq!(client.quick(), Err(IpcError::Timeout));
}

#[test]
fn with_wait_ns_builder_overrides_default() {
    let (client_end, _server_end) = MockEnd::pair();
    let client = CalcClient::new(client_end).with_wait_ns(0);
    assert_eq!(client.wait_ns(), 0);
    // Сервера нет -> two-way call истекает по тайм-ауту.
    assert_eq!(client.add(1, 2), Err(IpcError::Timeout));
}
