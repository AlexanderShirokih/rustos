//! Интеграция кодогена `protocol!`: клиент, диспетчер и сервис над хостовым каналом
//! `ipc-test`. Покрывает ответы обеих форм, отказ незнакомой операции, судьбу владеющих значений,
//! пересылку долга ответа и сессии протокола в ответе, агрегате и параметре.

#![allow(
    clippy::cast_possible_truncation,
    clippy::expect_used,
    clippy::std_instead_of_core
)]

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

use ipc::{
    Channel, FrameBytes, Incoming, ReceivedResources, ReplyChannel, Timeout,
    wire::{Cap, Header, IpcError, MESSAGE_INLINE_MAX, MessageBuf, Str, value},
};
use ipc_test::{MockChannel, MockReply, MockResources};
use syscall::{Handle, ObjectKind, kind};

fn cap_of<K: ObjectKind>(raw: u32) -> Cap<K> {
    Cap::from_handle(Handle::from_abi(
        std::num::NonZeroU32::new(raw).expect("non-zero"),
    ))
}

fn frame_of(ordinal: u64, fields: &[u32]) -> MessageBuf {
    let mut frame = MessageBuf::new();
    frame
        .write_header(&Header::new(ordinal))
        .expect("header ok");
    for (field_id, &field) in (1u8..).zip(fields) {
        frame
            .write_field(field_id, &value::encode_u32(field))
            .expect("field ok");
    }
    frame.finish().expect("finish ok");
    frame
}

type Dispatch<S> = fn(&mut S, Incoming<'_, MockResources>) -> Result<(), IpcError>;

fn serve<S: Send, R: Send>(
    mut service: S,
    frames: usize,
    dispatch: Dispatch<S>,
    client: impl FnOnce(MockChannel) -> R + Send,
) -> (R, S) {
    let (channel, listener) = ipc_test::pair();
    thread::scope(|scope| {
        let server = scope.spawn(move || {
            for _ in 0..frames {
                listener
                    .serve(|frame| dispatch(&mut service, frame))
                    .expect("dispatch ok");
            }
            service
        });
        let result = client(channel);
        (result, server.join().expect("server thread"))
    })
}

fn serve_abandoned<S>(
    service: &mut S,
    dispatch: Dispatch<S>,
    call: impl FnOnce(MockChannel),
) -> Result<(), IpcError> {
    let (channel, listener) = ipc_test::pair();
    call(channel);
    listener.serve(|frame| dispatch(service, frame))
}

const ERR_DIVIDE_BY_ZERO: u32 = 1;

ipc::protocol! {
    // Контракт со значениями ответа и односторонней операцией.
    mod calc {
        name = "Calc";

        call fn add(a: u32, b: u32) -> u32;
        call fn div(a: u32, b: u32) -> Result<u32, u32>;
        call fn echo_len(text: Str<16>) -> u32;
        cast fn reset(seed: u64);
    }
}

#[derive(Default)]
struct CalcServer {
    seed: u64,
    sums: usize,
}

impl<F: ReceivedResources> calc::Service<F> for CalcServer {
    fn add(&mut self, _frame: &F, a: u32, b: u32) -> u32 {
        self.sums += 1;
        a + b
    }

    fn div(&mut self, _frame: &F, a: u32, b: u32) -> Result<u32, u32> {
        a.checked_div(b).ok_or(ERR_DIVIDE_BY_ZERO)
    }

    fn echo_len(&mut self, _frame: &F, text: Str<16>) -> u32 {
        text.as_str().len() as u32
    }

    fn reset(&mut self, _frame: &F, seed: u64) {
        self.seed = seed;
    }
}

#[test]
fn operations_round_trip_through_the_dispatcher() {
    let (answers, server) = serve(CalcServer::default(), 4, calc::dispatch, |channel| {
        let client = calc::Client::new(channel);
        let sum = client.add(7, 35).expect("add");
        let refused = client.div(1, 0).expect("div");
        let len = client
            .echo_len(Str::new("hello").expect("within bound"))
            .expect("echo_len");
        client.reset(0xDEAD_BEEF).expect("reset");
        (sum, refused, len)
    });

    assert_eq!(answers, (42, Err(ERR_DIVIDE_BY_ZERO), 5));
    assert_eq!(server.seed, 0xDEAD_BEEF);
}

#[test]
fn ordinal_constants_match_golden() {
    assert_eq!(calc::add::ORDINAL, 0xb744_87cd_9b28_41ba);
    assert_eq!(calc::div::ORDINAL, 0x2420_7b95_a0d9_84fc);
    assert_eq!(calc::reset::ORDINAL, 0x1cb8_08a6_5161_cd40);
}

ipc::protocol! {
    // Тот же контракт под другим Rust-путём: идентичность берётся из ключа `name`.
    mod renamed_calc {
        name = "Calc";

        call fn add(a: u32, b: u32) -> u32;
    }
}

ipc::protocol! {
    // Одноимённая операция в другом namespace.
    mod other_calc {
        name = "other.Calc";

        call fn add(a: u32, b: u32) -> u32;
    }
}

#[test]
fn the_protocol_identity_comes_from_the_declared_name() {
    assert_eq!(
        <renamed_calc::Protocol as ipc::Protocol>::ID,
        <calc::Protocol as ipc::Protocol>::ID
    );
    assert_eq!(renamed_calc::add::ORDINAL, calc::add::ORDINAL);

    assert_ne!(
        <other_calc::Protocol as ipc::Protocol>::ID,
        <calc::Protocol as ipc::Protocol>::ID
    );
    assert_ne!(other_calc::add::ORDINAL, calc::add::ORDINAL);
}

#[test]
fn a_call_of_an_unknown_operation_is_refused() {
    let (answer, _) = serve(CalcServer::default(), 1, calc::dispatch, |channel| {
        let frame = frame_of(0xDEAD_DEAD_DEAD_DEAD, &[]);
        let mut reply = [0u8; MESSAGE_INLINE_MAX];
        channel
            .call(frame.as_bytes(), &[], &mut reply, Timeout::INFINITE)
            .map(|answer| answer.bytes.len())
    });

    assert_eq!(answer, Err(IpcError::PeerClosed));
}

#[test]
fn a_call_frame_without_a_reply_channel_reaches_no_handler() {
    let ((), server) = serve(CalcServer::default(), 1, calc::dispatch, |channel| {
        let frame = frame_of(calc::add::ORDINAL, &[2, 3]);
        channel
            .send(frame.as_bytes(), &[], Timeout::INFINITE)
            .expect("send ok");
    });

    assert_eq!(server.sums, 0);
}

#[test]
fn an_answer_of_another_operation_is_refused() {
    let (channel, listener) = ipc_test::pair();
    thread::scope(|scope| {
        scope.spawn(|| {
            listener.serve(|mut frame| {
                let reply = frame.resources.take_reply().expect("a call brings a reply");
                let answer = frame_of(calc::div::ORDINAL, &[999]);
                reply
                    .send(answer.as_bytes(), &[])
                    .expect("the caller waits");
            });
        });

        assert_eq!(
            calc::Client::new(channel).add(0, 0),
            Err(IpcError::InvalidValue)
        );
    });
}

ipc::protocol! {
    // Контракт с необязательным значением в ответе.
    mod lookup {
        name = "Lookup";

        call fn find(key: u32) -> Result<Option<u32>, u16>;
    }
}

struct LookupServer;

impl<F: ReceivedResources> lookup::Service<F> for LookupServer {
    fn find(&mut self, _frame: &F, key: u32) -> Result<Option<u32>, u16> {
        Ok((key == 1).then_some(7))
    }
}

#[test]
fn an_optional_answer_arrives_in_both_states() {
    let (answers, _) = serve(LookupServer, 2, lookup::dispatch, |channel| {
        let client = lookup::Client::new(channel);
        (client.find(1).expect("find"), client.find(2).expect("find"))
    });

    assert_eq!(answers, (Ok(Some(7)), Ok(None)));
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

ipc::protocol! {
    // Контракт с агрегатами, включая вложенный и заимствующий.
    mod geo {
        name = "Geo";

        call fn flip(s: Segment) -> Segment;
        call fn label_len(t: Tagged) -> u32;
    }
}

struct GeoServer;

impl<F: ReceivedResources> geo::Service<F> for GeoServer {
    fn flip(&mut self, _frame: &F, s: Segment) -> Segment {
        Segment {
            from: s.to,
            to: s.from,
        }
    }

    fn label_len(&mut self, _frame: &F, t: Tagged<'_>) -> u32 {
        t.label.as_str().len() as u32 + t.value
    }
}

#[test]
fn nested_and_borrowing_aggregates_round_trip() {
    let (answers, _) = serve(GeoServer, 2, geo::dispatch, |channel| {
        let client = geo::Client::new(channel);
        let flipped = client
            .flip(Segment {
                from: Point { x: 1, y: 2 },
                to: Point { x: 3, y: 4 },
            })
            .expect("flip ok");
        let tagged = Tagged {
            label: Str::new("ab").expect("within bound"),
            value: 40,
        };
        (flipped, client.label_len(tagged).expect("label_len ok"))
    });

    assert_eq!(
        answers,
        (
            Segment {
                from: Point { x: 3, y: 4 },
                to: Point { x: 1, y: 2 },
            },
            42
        )
    );
}

ipc::wire_value! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Vended<S = ipc::wire::Cap<kind::Port>> {
        session: S,
        prefix_len: u32,
    }
}

ipc::protocol! {
    // Контракт с агрегатом, содержащим capability.
    mod vendor {
        name = "Vendor";

        call fn vend(prefix_len: u32) -> Vended;
    }
}

const STAGED_CAP: u32 = 0x9191;

struct TrackedResources {
    caps: Vec<Option<TrackedCap>>,
}

struct TrackedCap {
    closed: Arc<AtomicUsize>,
}

impl Drop for TrackedCap {
    fn drop(&mut self) {
        self.closed.fetch_add(1, Ordering::SeqCst);
    }
}

impl ReceivedResources for TrackedResources {
    type Handle<K: ObjectKind> = TrackedCap;
    type Reply = core::convert::Infallible;

    fn take_handle<K: ObjectKind>(&mut self, index: u8) -> Option<TrackedCap> {
        self.caps.get_mut(usize::from(index))?.take()
    }

    fn reply_channel(_handle: TrackedCap) -> Option<Self::Reply> {
        None
    }

    fn take_reply(&mut self) -> Option<Self::Reply> {
        None
    }
}

struct IncompleteVend {
    closed: Arc<AtomicUsize>,
}

impl Channel for IncompleteVend {
    type Resources = TrackedResources;

    fn send(&self, _bytes: &[u8], _handles: &[u32], _wait: Timeout) -> Result<(), IpcError> {
        Err(IpcError::PeerClosed)
    }

    fn call<'b>(
        &self,
        bytes: &[u8],
        _handles: &[u32],
        reply: &'b mut FrameBytes,
        _wait: Timeout,
    ) -> Result<Incoming<'b, TrackedResources>, IpcError> {
        let header = Header::split(bytes)?.header;
        let mut frame = MessageBuf::new();
        frame.write_header(&header)?;
        frame.write_nested(1, |vended| vended.write_handle(1, STAGED_CAP))?;
        frame.finish()?;
        let bytes = reply
            .get_mut(..frame.len())
            .ok_or(IpcError::FrameOverflow)?;
        bytes.copy_from_slice(frame.as_bytes());
        Ok(Incoming {
            bytes,
            resources: TrackedResources {
                caps: vec![Some(TrackedCap {
                    closed: Arc::clone(&self.closed),
                })],
            },
        })
    }
}

/// Отказ разбора после уже взятой capability агрегата закрывает ресурсы кадра целиком.
#[test]
fn an_aggregate_decode_failure_closes_the_delivered_capability() {
    let closed = Arc::new(AtomicUsize::new(0));
    let client = vendor::Client::new(IncompleteVend {
        closed: Arc::clone(&closed),
    });

    let refused = client.vend(7).err();

    assert_eq!(refused, Some(IpcError::MissingField));
    assert_eq!(closed.load(Ordering::SeqCst), 1);
}

struct TrackedHandle {
    raw: u32,
    fates: Fates,
}

impl Drop for TrackedHandle {
    fn drop(&mut self) {
        self.fates.drops.fetch_add(1, Ordering::SeqCst);
    }
}

impl ipc::IntoWireValue for TrackedHandle {
    type Read = Cap<kind::Port>;

    fn write_as_field(&self, buf: &mut MessageBuf, field_id: u8) -> Result<(), IpcError> {
        buf.write_handle(field_id, self.raw)
    }

    fn relinquish(self) {
        self.fates.relinquishes.fetch_add(1, Ordering::SeqCst);
        core::mem::forget(self);
    }
}

#[derive(Default, Clone)]
struct Fates {
    drops: Arc<AtomicUsize>,
    relinquishes: Arc<AtomicUsize>,
}

impl Fates {
    fn handle(&self, raw: u32) -> TrackedHandle {
        TrackedHandle {
            raw,
            fates: self.clone(),
        }
    }

    fn counts(&self) -> (usize, usize) {
        (
            self.drops.load(Ordering::SeqCst),
            self.relinquishes.load(Ordering::SeqCst),
        )
    }
}

ipc::wire_value! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Lease<S = ipc::wire::Cap<kind::Port>> {
        session: S,
        len: u32,
    }
}

ipc::protocol! {
    // Контракт владеющих параметров и ответов.
    mod lender {
        name = "Lender";

        call fn keep(own capability: ipc::wire::Cap<kind::Port>) -> u32;
        call fn lend() -> own ipc::wire::Cap<kind::Port>;
        call fn lease(len: u32) -> own Lease;
    }
}

ipc::protocol! {
    // Контракт односторонней передачи владения capability.
    mod owned_cast {
        name = "OwnedCast";

        cast fn deliver(own capability: ipc::wire::Cap<kind::Port>);
    }
}

struct Lender {
    fates: Fates,
}

impl lender::Service<MockResources> for Lender {
    fn keep(&mut self, _frame: &MockResources, capability: Cap<kind::Port>) -> u32 {
        capability.raw().get()
    }

    fn lend(&mut self, _frame: &MockResources) -> impl ipc::IntoWireValue<Read = Cap<kind::Port>> {
        self.fates.handle(0x61)
    }

    fn lease(&mut self, _frame: &MockResources, len: u32) -> impl ipc::IntoWireValue<Read = Lease> {
        Lease {
            session: self.fates.handle(0x62),
            len,
        }
    }
}

#[derive(Default)]
struct Keeper {
    claimed: Option<Cap<kind::Port>>,
}

impl owned_cast::Service<MockResources> for Keeper {
    fn deliver(&mut self, _frame: &MockResources, capability: Cap<kind::Port>) {
        self.claimed = Some(capability);
    }
}

#[test]
fn a_delivered_owned_parameter_is_relinquished() {
    let fates = Fates::default();

    let (kept, _) = serve(
        Lender {
            fates: fates.clone(),
        },
        1,
        lender::dispatch,
        |channel| lender::Client::new(channel).keep(fates.handle(0x41)),
    );
    let ((), keeper) = serve(Keeper::default(), 1, owned_cast::dispatch, |channel| {
        owned_cast::Client::new(channel)
            .deliver(fates.handle(0x42))
            .expect("cast ok");
    });

    assert_eq!(kept, Ok(0x41));
    assert_eq!(keeper.claimed, Some(cap_of(0x42)));
    assert_eq!(fates.counts(), (0, 2));
}

#[test]
fn an_unsent_one_way_parameter_comes_back_to_its_sender() {
    let fates = Fates::default();
    let (channel, listener) = ipc_test::pair();
    drop(listener);

    let refused = owned_cast::Client::new(channel)
        .deliver(fates.handle(0x43))
        .expect_err("the cast is refused");

    assert_eq!(refused.error, IpcError::PeerClosed);
    assert_eq!(refused.value.raw, 0x43);
    assert_eq!(fates.counts(), (0, 0));
    drop(refused);
    assert_eq!(fates.counts(), (1, 0));
}

#[test]
fn an_unsent_call_parameter_closes_its_handle() {
    let fates = Fates::default();
    let (channel, listener) = ipc_test::pair();
    drop(listener);

    let refused = lender::Client::new(channel).keep(fates.handle(0x44));

    assert_eq!(refused, Err(IpcError::PeerClosed));
    assert_eq!(fates.counts(), (1, 0));
}

#[test]
fn a_delivered_owned_answer_is_relinquished_with_its_aggregate() {
    let fates = Fates::default();

    let (answers, _) = serve(
        Lender {
            fates: fates.clone(),
        },
        2,
        lender::dispatch,
        |channel| {
            let client = lender::Client::new(channel);
            let lent = client.lend().expect("lend ok");
            let leased = client.lease(3).expect("lease ok");
            (lent, leased)
        },
    );

    assert_eq!(
        answers,
        (
            cap_of(0x61),
            Lease {
                session: cap_of(0x62),
                len: 3,
            }
        )
    );
    assert_eq!(fates.counts(), (0, 2));
}

#[test]
fn an_undelivered_owned_answer_closes_its_handle_and_fails_the_dispatch() {
    let fates = Fates::default();
    let mut server = Lender {
        fates: fates.clone(),
    };

    let dispatched = serve_abandoned(&mut server, lender::dispatch, |channel| {
        let client = lender::Client::new(channel).with_wait(Timeout::POLL);
        assert!(matches!(client.lend(), Err(IpcError::Timeout)));
    });

    assert_eq!(dispatched, Err(IpcError::PeerClosed));
    assert_eq!(fates.counts(), (1, 0));
}

ipc::wire_value! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Bundle<S = ipc::wire::Session<crate::dealer::Protocol>> {
        session: S,
        len: u32,
    }
}

ipc::protocol! {
    // Контракт, который выдаёт и принимает сессии самого себя.
    mod dealer {
        name = "Dealer";

        call fn open(key: u32) -> own Result<ipc::wire::Session<crate::dealer::Protocol>, u16>;
        call fn bundle(len: u32) -> own Bundle;
        call fn take(own session: ipc::wire::Session<crate::dealer::Protocol>) -> u32;
    }
}

type DealerSession = ipc::wire::Session<dealer::Protocol>;

struct Dealer;

impl dealer::Service<MockResources> for Dealer {
    fn open(
        &mut self,
        _frame: &MockResources,
        key: u32,
    ) -> impl ipc::IntoWireValue<Read = Result<DealerSession, u16>> {
        if key == 0 {
            return Err(3);
        }
        Ok(DealerSession::from_handle(cap_of(0x50 + key)))
    }

    fn bundle(
        &mut self,
        _frame: &MockResources,
        len: u32,
    ) -> impl ipc::IntoWireValue<Read = Bundle> {
        Bundle {
            session: DealerSession::from_handle(cap_of(0x5A)),
            len,
        }
    }

    fn take(&mut self, _frame: &MockResources, session: DealerSession) -> u32 {
        session.into_handle().raw().get()
    }
}

#[test]
fn a_session_reply_round_trips() {
    let (answers, _) = serve(Dealer, 2, dealer::dispatch, |channel| {
        let client = dealer::Client::new(channel);
        (client.open(1).expect("open"), client.open(0).expect("open"))
    });

    assert_eq!(
        answers,
        (Ok(DealerSession::from_handle(cap_of(0x51))), Err(3))
    );
}

#[test]
fn a_session_aggregate_field_round_trips() {
    let (bundled, _) = serve(Dealer, 1, dealer::dispatch, |channel| {
        dealer::Client::new(channel).bundle(9)
    });

    assert_eq!(
        bundled,
        Ok(Bundle {
            session: DealerSession::from_handle(cap_of(0x5A)),
            len: 9,
        })
    );
}

#[test]
fn a_session_parameter_is_typed_as_a_capability() {
    let (taken, _) = serve(Dealer, 1, dealer::dispatch, |channel| {
        dealer::Client::new(channel).take(DealerSession::from_handle(cap_of(0x5C)))
    });

    assert_eq!(taken, Ok(0x5C));
    assert_eq!(
        <DealerSession as ipc::WireTyped>::WIRE_TYPE,
        ipc::schema::WireType::Capability
    );
}

ipc::protocol! {
    // Контракт отложенных ответов.
    mod ledger {
        name = "Ledger";

        call(defer) fn hold(tag: u32) -> u32;
        call(defer) fn grant() -> own ipc::wire::Cap<kind::Port>;
    }
}

#[derive(Default)]
struct Ledger {
    held: Vec<(u32, ledger::hold::Reply<MockReply>)>,
    granted: Vec<ledger::grant::Reply<MockReply>>,
}

impl ledger::Service<MockResources> for Ledger {
    fn hold(&mut self, _frame: &MockResources, tag: u32, reply: ledger::hold::Reply<MockReply>) {
        self.held.push((tag, reply));
    }

    fn grant(&mut self, _frame: &MockResources, reply: ledger::grant::Reply<MockReply>) {
        self.granted.push(reply);
    }
}

ipc::protocol! {
    // Контракт передачи долга другому держателю.
    mod relay {
        name = "Relay";

        cast fn forward(own caller: ipc::Reply<u32>);
    }
}

#[derive(Default)]
struct Relayed {
    caller: Option<ipc::Reply<u32, MockReply>>,
}

impl relay::Service<MockResources> for Relayed {
    fn forward(&mut self, _frame: &MockResources, caller: ipc::Reply<u32, MockReply>) {
        self.caller = Some(caller);
    }
}

fn hold_and_settle(
    settle: impl FnOnce(ledger::hold::Reply<MockReply>) + Send,
) -> Result<u32, IpcError> {
    let (channel, listener) = ipc_test::pair();
    thread::scope(|scope| {
        scope.spawn(move || {
            let mut ledger = Ledger::default();
            listener
                .serve(|frame| ledger::dispatch(&mut ledger, frame))
                .expect("dispatch ok");
            let (tag, reply) = ledger.held.pop().expect("the debt is kept");
            assert_eq!(tag, 41);
            settle(reply);
        });
        ledger::Client::new(channel).hold(41)
    })
}

#[test]
fn a_kept_debt_answers_after_dispatch_returns() {
    let answer = hold_and_settle(|reply| reply.send(42u32).expect("the caller waits"));

    assert_eq!(answer, Ok(42));
}

#[test]
fn a_dropped_debt_refuses_its_caller() {
    assert_eq!(hold_and_settle(drop), Err(IpcError::PeerClosed));
}

#[test]
fn a_forwarded_debt_answers_its_original_caller() {
    let answer = hold_and_settle(|debt| {
        let (relay_channel, relay_listener) = ipc_test::pair();
        relay::Client::new(relay_channel)
            .forward(debt)
            .expect("the debt is forwarded");
        let mut relayed = Relayed::default();
        relay_listener
            .serve(|frame| relay::dispatch(&mut relayed, frame))
            .expect("relay dispatch ok");

        relayed
            .caller
            .expect("the debt arrives")
            .send(7u32)
            .expect("the new holder answers");
    });

    assert_eq!(answer, Ok(7));
}

#[test]
fn an_undelivered_debt_hands_its_value_back() {
    let fates = Fates::default();
    let mut ledger = Ledger::default();
    serve_abandoned(&mut ledger, ledger::dispatch, |channel| {
        let client = ledger::Client::new(channel).with_wait(Timeout::POLL);
        assert!(matches!(client.grant(), Err(IpcError::Timeout)));
    })
    .expect("dispatch ok");
    let reply = ledger.granted.pop().expect("the debt is kept");

    let refused = reply
        .send(fates.handle(0x71))
        .expect_err("the caller is gone");

    assert_eq!(refused.error, IpcError::PeerClosed);
    assert_eq!(refused.value.raw, 0x71);
    assert_eq!(fates.counts(), (0, 0));
    drop(refused);
    assert_eq!(fates.counts(), (1, 0));
}
