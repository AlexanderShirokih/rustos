//! Интеграция кодогена `protocol!`: клиент, диспетчер и сервис над хостовым каналом
//! `ipc-test`. Покрывает ответы обеих форм, отказ незнакомой операции, судьбу владеющих значений,
//! пересылку долга ответа и сессии протокола в ответе, агрегате и параметре.

#![allow(
    clippy::cast_possible_truncation,
    clippy::expect_used,
    clippy::std_instead_of_core
)]

use std::{
    cell::RefCell,
    pin::pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
    thread,
};

use ipc::{
    CallError, Channel, Incoming, Outgoing, ReceivedResources, ReplyChannel, Timeout, Unsent,
    wire::{Cap, Header, IpcError, MessageBuf, Session, value},
};
use ipc_test::{MockChannel, MockHandle, MockReply, MockResources};
use syscall::{ObjectKind, kind};

fn cap_of<K: ObjectKind>(raw: u32) -> MockHandle<K> {
    MockHandle::new(raw)
}

fn frame_of<R: ReceivedResources>(ordinal: u64, fields: &[u32]) -> MessageBuf<R> {
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

fn complete<F: Future>(future: F) -> F::Output {
    match pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(output) => Some(output),
        Poll::Pending => None,
    }
    .expect("a mock channel operation completes at once")
}

const ERR_DIVIDE_BY_ZERO: u32 = 1;

ipc::protocol! {
    // Контракт со значениями ответа и односторонней операцией.
    mod calc {
        name = "Calc";

        call fn add(a: u32, b: u32) -> u32;
        call fn div(a: u32, b: u32) -> Result<u32, u32>;
        call fn echo_len(text: &str) -> u32;
        call fn joined_len(head: &str, tail: &str) -> u32;
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

    fn echo_len(&mut self, _frame: &F, text: &str) -> u32 {
        text.len() as u32
    }

    fn joined_len(&mut self, _frame: &F, head: &str, tail: &str) -> u32 {
        (head.len() + tail.len()) as u32
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
        let refused = client.div(1, 0);
        let len = client.echo_len("hello").expect("echo_len");
        client.reset(0xDEAD_BEEF).expect("reset");
        (sum, refused, len)
    });

    assert_eq!(answers, (42, Err(CallError::Remote(ERR_DIVIDE_BY_ZERO)), 5));
    assert_eq!(server.seed, 0xDEAD_BEEF);
}

#[test]
fn the_async_client_answers_as_the_sync_client() {
    let (answers, _) = serve(CalcServer::default(), 8, calc::dispatch, |channel| {
        let client = calc::Client::new(channel.clone());
        let sync = (
            client.add(7, 35),
            client.div(1, 0),
            client.echo_len("hello"),
            client.reset(1),
        );
        let client = calc::AsyncClient::new(channel);
        let asynchronous = (
            complete(client.add(7, 35)),
            complete(client.div(1, 0)),
            complete(client.echo_len("hello")),
            complete(client.reset(1)),
        );
        (sync, asynchronous)
    });

    assert_eq!(answers.0, answers.1);
    assert_eq!(answers.1.1, Err(CallError::Remote(ERR_DIVIDE_BY_ZERO)));
}

#[test]
fn a_string_is_bounded_only_by_the_frame() {
    let long = "s".repeat(3000);
    let (answers, _) = serve(CalcServer::default(), 1, calc::dispatch, |channel| {
        let client = calc::Client::new(channel);
        (client.echo_len(&long), client.joined_len(&long, &long))
    });

    assert_eq!(answers, (Ok(3000), Err(IpcError::FrameOverflow)));
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
        let mut frame = frame_of(0xDEAD_DEAD_DEAD_DEAD, &[]);
        channel
            .call(frame.outgoing(), Timeout::INFINITE)
            .map(|answer| answer.bytes.len())
    });

    assert_eq!(answer, Err(IpcError::PeerClosed));
}

#[test]
fn a_call_frame_without_a_reply_channel_reaches_no_handler() {
    let ((), server) = serve(CalcServer::default(), 1, calc::dispatch, |channel| {
        let mut frame = frame_of(calc::add::ORDINAL, &[2, 3]);
        channel
            .send(frame.outgoing(), Timeout::INFINITE)
            .map_err(|(error, _)| error)
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
                let mut answer = frame_of(calc::div::ORDINAL, &[999]);
                reply
                    .send(answer.outgoing())
                    .map_err(|(error, _)| error)
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
        (client.find(1), client.find(2))
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
        label: &'a str,
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
        t.label.len() as u32 + t.value
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
            label: "ab",
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
    pub struct Vended<S = ipc::wire::Cap<kind::Port>> {
        session: S,
        prefix_len: u32,
    }
}

ipc::protocol! {
    // Контракт с агрегатом, содержащим capability.
    mod vendor {
        name = "Vendor";

        call fn vend(prefix_len: u32) -> own Vended;
    }
}

#[derive(Default)]
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

enum NoReply {}

impl ReplyChannel for NoReply {
    type Resources = TrackedResources;

    fn send(
        self,
        _frame: Outgoing<'_, TrackedResources>,
    ) -> Result<(), Unsent<'_, TrackedResources>> {
        match self {}
    }
}

impl ReceivedResources for TrackedResources {
    type Handle<K: ObjectKind> = TrackedCap;
    type Reply = NoReply;

    fn take_handle<K: ObjectKind>(&mut self, index: u8) -> Option<TrackedCap> {
        self.caps.get_mut(usize::from(index))?.take()
    }

    fn put_handle<K: ObjectKind>(&mut self, handle: TrackedCap) -> Option<u8> {
        let index = u8::try_from(self.caps.len()).ok()?;
        self.caps.push(Some(handle));
        Some(index)
    }

    fn reply_channel(_handle: TrackedCap) -> Option<NoReply> {
        None
    }

    fn take_reply(&mut self) -> Option<NoReply> {
        None
    }
}

struct IncompleteVend {
    closed: Arc<AtomicUsize>,
}

impl Channel for IncompleteVend {
    type Resources = TrackedResources;

    fn send<'b>(
        &self,
        frame: Outgoing<'b, TrackedResources>,
        _wait: Timeout,
    ) -> Result<(), Unsent<'b, TrackedResources>> {
        Err((IpcError::PeerClosed, frame))
    }

    fn call<'b>(
        &self,
        frame: Outgoing<'b, TrackedResources>,
        _wait: Timeout,
    ) -> Result<Incoming<'b, TrackedResources>, IpcError> {
        let header = Header::split(frame.as_bytes())?.header;
        let reply = frame.into_parts().buffer;
        let mut answer = MessageBuf::<TrackedResources>::new();
        answer.write_header(&header)?;
        let staged = TrackedCap {
            closed: Arc::clone(&self.closed),
        };
        answer.write_nested(1, |vended| vended.write_handle::<kind::Port>(1, staged))?;
        answer.finish()?;
        let bytes = reply
            .get_mut(..answer.len())
            .ok_or(IpcError::FrameOverflow)?;
        bytes.copy_from_slice(answer.as_bytes());
        Ok(Incoming {
            bytes,
            resources: answer.into_resources(),
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

#[derive(Default, Clone)]
struct Fates {
    closed: Arc<AtomicUsize>,
}

impl Fates {
    fn handle(&self, raw: u32) -> MockHandle<kind::Port> {
        MockHandle::tracked(raw, &self.closed)
    }

    fn closed(&self) -> usize {
        self.closed.load(Ordering::SeqCst)
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
        cast fn deliver_pair(own first: ipc::wire::Cap<kind::Port>, own second: ipc::wire::Cap<kind::Port>);
    }
}

struct Lender {
    fates: Fates,
}

impl lender::Service<MockResources> for Lender {
    fn keep(&mut self, _frame: &MockResources, capability: MockHandle<kind::Port>) -> u32 {
        capability.raw()
    }

    fn lend(
        &mut self,
        _frame: &MockResources,
    ) -> impl ipc::IntoWireValue<MockResources, Read = Cap<kind::Port>> {
        self.fates.handle(0x61)
    }

    fn lease(
        &mut self,
        _frame: &MockResources,
        len: u32,
    ) -> impl ipc::IntoWireValue<MockResources, Read = Lease> {
        Lease {
            session: self.fates.handle(0x62),
            len,
        }
    }
}

#[derive(Default)]
struct Keeper {
    claimed: Vec<MockHandle<kind::Port>>,
}

impl owned_cast::Service<MockResources> for Keeper {
    fn deliver(&mut self, _frame: &MockResources, capability: MockHandle<kind::Port>) {
        self.claimed.push(capability);
    }

    fn deliver_pair(
        &mut self,
        _frame: &MockResources,
        first: MockHandle<kind::Port>,
        second: MockHandle<kind::Port>,
    ) {
        self.claimed.extend([first, second]);
    }
}

#[test]
fn a_delivered_owned_parameter_stays_open_at_its_sender() {
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
    assert_eq!(keeper.claimed, [cap_of(0x42)]);
    assert_eq!(fates.closed(), 0);
}

#[test]
fn an_unsent_one_way_parameter_comes_back_as_its_receiver_reads_it() {
    let fates = Fates::default();
    let (channel, listener) = ipc_test::pair();
    drop(listener);

    let refused = owned_cast::Client::new(channel)
        .deliver(fates.handle(0x43))
        .expect_err("the cast is refused");

    assert_eq!(refused.error, IpcError::PeerClosed);
    let capability = refused.into_value().expect("the frame is read");
    assert_eq!(capability, cap_of(0x43));
    assert_eq!(fates.closed(), 0);
    drop(capability);
    assert_eq!(fates.closed(), 1);
}

#[test]
fn unsent_one_way_parameters_come_back_in_declaration_order() {
    let fates = Fates::default();
    let (channel, listener) = ipc_test::pair();
    drop(listener);

    let refused = owned_cast::Client::new(channel)
        .deliver_pair(fates.handle(0x45), fates.handle(0x46))
        .expect_err("the cast is refused");

    assert_eq!(refused.into_value(), Ok((cap_of(0x45), cap_of(0x46))));
    assert_eq!(fates.closed(), 2);
}

#[test]
fn an_unsent_async_one_way_parameter_comes_back_as_the_sync_one() {
    let fates = Fates::default();
    let (channel, listener) = ipc_test::pair();
    drop(listener);

    let client = owned_cast::AsyncClient::new(channel);
    let refused = complete(client.deliver_pair(fates.handle(0x4C), fates.handle(0x4D)))
        .expect_err("the cast is refused");
    let unsent_call =
        complete(lender::AsyncClient::new(client.into_channel()).keep(fates.handle(0x4E)));

    assert_eq!(refused.error, IpcError::PeerClosed);
    assert_eq!(unsent_call, Err(IpcError::PeerClosed));
    assert_eq!(fates.closed(), 1);
    assert_eq!(refused.into_value(), Ok((cap_of(0x4C), cap_of(0x4D))));
    assert_eq!(fates.closed(), 3);
}

#[test]
fn a_dropped_undelivered_closes_its_handles() {
    let fates = Fates::default();
    let (channel, listener) = ipc_test::pair();
    drop(listener);

    let refused = owned_cast::Client::new(channel)
        .deliver_pair(fates.handle(0x47), fates.handle(0x48))
        .expect_err("the cast is refused");

    assert_eq!(fates.closed(), 0);
    drop(refused);
    assert_eq!(fates.closed(), 2);
}

struct Unwritable;

impl ipc::IntoWireValue<MockResources> for Unwritable {
    type Read = Cap<kind::Port>;

    fn write_into(
        self,
        _frame: &mut MessageBuf<MockResources>,
        _field_id: u8,
    ) -> Result<(), IpcError> {
        Err(IpcError::InvalidValue)
    }
}

#[test]
fn a_frame_refused_while_written_closes_its_written_handles_at_once() {
    let fates = Fates::default();
    let (channel, _listener) = ipc_test::pair();

    let refused = owned_cast::Client::new(channel)
        .deliver_pair(fates.handle(0x49), Unwritable)
        .expect_err("the second parameter is not written");

    assert_eq!(refused.error, IpcError::InvalidValue);
    assert_eq!(fates.closed(), 1);
    assert_eq!(refused.into_value(), Err(IpcError::InvalidValue));
}

#[test]
fn a_refusal_closes_the_handles_its_value_leaves_unnamed() {
    let fates = Fates::default();
    let mut frame = MessageBuf::<MockResources>::new();
    frame
        .write_header(&Header::new(owned_cast::deliver::ORDINAL))
        .expect("header ok");
    frame.write_handle(1, fates.handle(0x4A)).expect("first ok");
    frame
        .write_handle(2, fates.handle(0x4B))
        .expect("second ok");
    frame.finish().expect("finish ok");

    let refused = ipc::Undelivered::__new(
        (IpcError::PeerClosed, frame.outgoing()),
        |body, resources| {
            let field = ipc::wire::FieldCursor::new(body)
                .next_field()?
                .ok_or(IpcError::MissingField)?;
            <Cap<kind::Port> as ipc::WireValue<'_, MockResources>>::from_field(
                field.data, resources,
            )
        },
    );

    assert_eq!(fates.closed(), 1);
    let capability = refused.into_value().expect("the frame is read");
    assert_eq!(capability, cap_of(0x4A));
    assert_eq!(fates.closed(), 1);
    drop(capability);
    assert_eq!(fates.closed(), 2);
}

#[test]
fn an_unsent_call_parameter_closes_its_handle() {
    let fates = Fates::default();
    let (channel, listener) = ipc_test::pair();
    drop(listener);

    let refused = lender::Client::new(channel).keep(fates.handle(0x44));

    assert_eq!(refused, Err(IpcError::PeerClosed));
    assert_eq!(fates.closed(), 1);
}

#[test]
fn a_delivered_owned_answer_stays_open_at_its_server() {
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
    assert_eq!(fates.closed(), 0);
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
    assert_eq!(fates.closed(), 1);
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

type DealerSession = Session<dealer::Protocol, MockHandle<kind::Port>>;

struct Dealer;

impl dealer::Service<MockResources> for Dealer {
    fn open(
        &mut self,
        _frame: &MockResources,
        key: u32,
    ) -> impl ipc::IntoWireValue<MockResources, Read = Result<Session<dealer::Protocol>, u16>> {
        if key == 0 {
            return Err(3);
        }
        Ok(DealerSession::from_handle(cap_of(0x50 + key)))
    }

    fn bundle(
        &mut self,
        _frame: &MockResources,
        len: u32,
    ) -> impl ipc::IntoWireValue<MockResources, Read = Bundle> {
        Bundle {
            session: DealerSession::from_handle(cap_of(0x5A)),
            len,
        }
    }

    fn take(&mut self, _frame: &MockResources, session: DealerSession) -> u32 {
        session.into_handle().raw()
    }
}

#[test]
fn a_session_reply_round_trips() {
    let (answers, _) = serve(Dealer, 2, dealer::dispatch, |channel| {
        let client = dealer::Client::new(channel);
        (client.open(1), client.open(0))
    });

    assert_eq!(
        answers,
        (
            Ok(DealerSession::from_handle(cap_of(0x51))),
            Err(CallError::Remote(3))
        )
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
        <Session<dealer::Protocol> as ipc::WireTyped>::WIRE_TYPE,
        ipc::schema::WireType::Capability
    );
}

ipc::protocol! {
    // Контракт отложенных ответов.
    mod ledger {
        name = "Ledger";

        call(defer) fn hold(tag: u32) -> u32;
        call(defer) fn grant() -> own ipc::wire::Cap<kind::Port>;
        call(defer) fn grant_lease() -> own Lease;
    }
}

#[derive(Default)]
struct Ledger {
    held: Vec<(u32, ledger::hold::Reply<MockReply>)>,
    granted: Vec<ledger::grant::Reply<MockReply>>,
    leased: Vec<ledger::grant_lease::Reply<MockReply>>,
}

impl ledger::Service<MockResources> for Ledger {
    fn hold(&mut self, _frame: &MockResources, tag: u32, reply: ledger::hold::Reply<MockReply>) {
        self.held.push((tag, reply));
    }

    fn grant(&mut self, _frame: &MockResources, reply: ledger::grant::Reply<MockReply>) {
        self.granted.push(reply);
    }

    fn grant_lease(
        &mut self,
        _frame: &MockResources,
        reply: ledger::grant_lease::Reply<MockReply>,
    ) {
        self.leased.push(reply);
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
    let capability = refused.into_value().expect("the frame is read");
    assert_eq!(capability, cap_of(0x71));
    assert_eq!(fates.closed(), 0);
    drop(capability);
    assert_eq!(fates.closed(), 1);
}

#[test]
fn an_undelivered_aggregate_answer_comes_back_as_its_caller_reads_it() {
    let fates = Fates::default();
    let mut ledger = Ledger::default();
    serve_abandoned(&mut ledger, ledger::dispatch, |channel| {
        let client = ledger::Client::new(channel).with_wait(Timeout::POLL);
        assert!(matches!(client.grant_lease(), Err(IpcError::Timeout)));
    })
    .expect("dispatch ok");
    let reply = ledger.leased.pop().expect("the debt is kept");

    let refused = reply
        .send(Lease {
            session: fates.handle(0x72),
            len: 5,
        })
        .expect_err("the caller is gone");

    let lease = refused.into_value().expect("the frame is read");
    assert_eq!(
        lease,
        Lease {
            session: cap_of(0x72),
            len: 5,
        }
    );
    assert_eq!(fates.closed(), 0);
    drop(lease);
    assert_eq!(fates.closed(), 1);
}

ipc::protocol! {
    mod waits {
        name = "Waits";

        call fn quick() -> u32;
        cast fn note();
    }
}

#[derive(Default)]
struct WaitRecorder {
    waits: RefCell<Vec<Timeout>>,
}

impl Channel for WaitRecorder {
    type Resources = MockResources;

    fn send<'b>(
        &self,
        frame: Outgoing<'b, MockResources>,
        wait: Timeout,
    ) -> Result<(), Unsent<'b, MockResources>> {
        self.waits.borrow_mut().push(wait);
        Err((IpcError::Timeout, frame))
    }

    fn call<'b>(
        &self,
        _frame: Outgoing<'b, MockResources>,
        wait: Timeout,
    ) -> Result<Incoming<'b, MockResources>, IpcError> {
        self.waits.borrow_mut().push(wait);
        Err(IpcError::Timeout)
    }
}

fn waits_of(client: waits::Client<WaitRecorder>) -> Vec<Timeout> {
    let _ = client.quick();
    let _ = client.note();
    client.into_channel().waits.into_inner()
}

#[test]
fn a_client_without_a_wait_waits_for_the_default_term() {
    let client = waits::Client::new(WaitRecorder::default());

    assert_eq!(waits_of(client), [ipc::DEFAULT_OPERATION_TIMEOUT; 2]);
}

#[test]
fn the_caller_wait_replaces_the_default_term() {
    let client = waits::Client::new(WaitRecorder::default()).with_wait(Timeout::INFINITE);

    assert_eq!(waits_of(client), [Timeout::INFINITE; 2]);
}
