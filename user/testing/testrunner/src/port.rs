//! E2E проверка synchronous рандеву-IPC из EL0:
//! `PortCreate`/`Send`/`Recv`/`Call`/`Reply`.
//!
//! Каждый тест использует два потока одного процесса. Простые body-кейсы
//! идут через сахар `Port::{send_bytes,recv_bytes,call_bytes}`/
//! `Reply::reply_bytes`. Сырой доступ к буферу (`write_msg`/`read_msg`/
//! `read_cap`/`read_badge`) остаётся там, где переносятся caps, читается badge
//! или поток держит сырой `Handle` из `EP_RAW` без обёртки.
//!
//! Покрытие:
//! - (a) send/recv round-trip тела;
//! - (b) call/reply round-trip;
//! - (c) перенос Signal-хендла через port и проверка идентичности
//!   объекта на стороне получателя (сигнал, поднятый до переноса, виден);
//! - (d) порядок "получатель пришёл первым" и "отправитель пришёл первым";
//! - (g) ожидание готовности приёма порта через `wait_any`.

use core::{
    num::NonZeroU32,
    sync::atomic::{AtomicI64, AtomicU64, Ordering::SeqCst},
};

use kernel_objects::{Error, Port, Signal, kind, wait_any};
use runtime::{
    JoinHandle, Timeout, handle_duplicate, ipc_buffer_addr, port_call, port_recv, port_send,
    signal_create, signal_set, signal_wait_one, spawn, spawn_owned,
};
use syscall::{
    Handle, READABLE, SIGNALED, SyscallError, WaitItem, WakeCount, decode_tag, encode_tag,
};

const JOIN_TIMEOUT: Timeout = Timeout::from_secs(5);

const CAPS_OFF: usize = core::mem::offset_of!(syscall::IpcBuffer, caps);
const DATA_OFF: usize = core::mem::offset_of!(syscall::IpcBuffer, data);
const BADGE_OFF: usize = core::mem::offset_of!(syscall::IpcBuffer, badge);

const RIGHTS_WRITE: u32 = 1 << 3;

static EP_RAW: AtomicU64 = AtomicU64::new(0);
static WORKER_RESULT: AtomicI64 = AtomicI64::new(i64::MIN);
static WORKER_READY: AtomicU64 = AtomicU64::new(0);
static NOTIF_RAW: AtomicU64 = AtomicU64::new(0);

fn ipc_va() -> usize {
    let ret = ipc_buffer_addr();
    assert_positive(ret);
    usize::try_from(ret).expect("positive va fits usize")
}

fn assert_positive(v: i64) {
    test_harness::check!(v > 0);
}

// SAFETY: ядро замаппило per-thread IPC-буфер user-RW (одна страница);
// запись tag/data лежит в его границах, поток - единственный владелец.
unsafe fn write_msg(va: usize, body: &[u8], caps: &[u32]) {
    // SAFETY: caller guarantees `va` points to the current thread's per-thread IPC-буфер.
    unsafe {
        let tag = va as *mut u64;
        tag.write_volatile(encode_tag(body.len(), caps.len()));
        let cap_ptr = (va + CAPS_OFF) as *mut u32;
        for (i, c) in caps.iter().enumerate() {
            cap_ptr.add(i).write_volatile(*c);
        }
        let data = (va + DATA_OFF) as *mut u8;
        for (i, b) in body.iter().enumerate() {
            data.add(i).write_volatile(*b);
        }
    }
}

// SAFETY: см. `write_msg`.
unsafe fn read_msg(va: usize, out: &mut [u8]) -> (usize, usize) {
    // SAFETY: caller guarantees `va` points to the current thread's per-thread IPC-буфер.
    unsafe {
        let tag = (va as *const u64).read_volatile();
        let decoded = decode_tag(tag);
        let data = (va + DATA_OFF) as *const u8;
        let n = out.len().min(decoded.len);
        for (i, slot) in out.iter_mut().enumerate().take(n) {
            *slot = data.add(i).read_volatile();
        }
        (decoded.len, decoded.ncaps)
    }
}

// SAFETY: см. `write_msg`.
unsafe fn read_cap(va: usize, i: usize) -> u32 {
    // SAFETY: caller guarantees `va` points to the current thread's per-thread IPC-буфер.
    unsafe { ((va + CAPS_OFF) as *const u32).add(i).read_volatile() }
}

// SAFETY: см. `write_msg`.
unsafe fn read_badge(va: usize) -> u64 {
    // SAFETY: caller guarantees `va` points to the current thread's per-thread IPC-буфер.
    unsafe { ((va + BADGE_OFF) as *const u64).read_volatile() }
}

fn port_of(raw: u32) -> Option<Handle<kind::Port>> {
    NonZeroU32::new(raw).map(Handle::from_abi)
}

fn ep_handle() -> Handle<kind::Port> {
    port_of(EP_RAW.load(SeqCst) as u32).expect("port handle present")
}

fn spawn_worker(entry: extern "C" fn(usize) -> u32) -> JoinHandle {
    WORKER_RESULT.store(i64::MIN, SeqCst);
    WORKER_READY.store(0, SeqCst);
    spawn(entry, 0).expect("spawn worker")
}

fn join(thread: JoinHandle) {
    thread
        .join(JOIN_TIMEOUT)
        .expect("worker joins before timeout");
}

// --- (a) + (d, sender-first): worker = sender, main = receiver. --------------

extern "C" fn sender_worker(_arg: usize) -> u32 {
    let va = ipc_va();
    // SAFETY: собственный per-thread буфер.
    unsafe {
        write_msg(va, b"ping", &[]);
    }
    WORKER_READY.store(1, SeqCst);
    let r = port_send(ep_handle(), Timeout::INFINITE);
    WORKER_RESULT.store(r, SeqCst);
    0
}

#[test_harness::test]
fn send_recv_round_trip_sender_first() {
    let ep = Port::create().expect("port create");
    EP_RAW.store(u64::from(ep.as_raw().raw()), SeqCst);

    let worker = spawn_worker(sender_worker);
    while WORKER_READY.load(SeqCst) == 0 {
        core::hint::spin_loop();
    }

    let mut buf = [0u8; 8];
    let (len, reply) = ep.recv_bytes(&mut buf, Timeout::INFINITE).expect("recv");
    test_harness::check!(reply.is_none());
    test_harness::check_eq!(len, 4);
    test_harness::check!(&buf[..4] == b"ping");

    join(worker);
    test_harness::check_eq!(WORKER_RESULT.load(SeqCst), 0);
}

// --- (d, receiver-first): worker = receiver, main = sender. ------------------

extern "C" fn receiver_worker(_arg: usize) -> u32 {
    let va = ipc_va();
    WORKER_READY.store(1, SeqCst);
    if !matches!(port_recv(ep_handle(), Timeout::INFINITE), Ok(None)) {
        WORKER_RESULT.store(-100, SeqCst);
        return 0;
    }
    let mut buf = [0u8; 8];
    // SAFETY: собственный буфер worker-потока.
    let (len, _ncaps) = unsafe { read_msg(va, &mut buf) };
    if len == 5 && &buf[..5] == b"hello" {
        WORKER_RESULT.store(0, SeqCst);
    } else {
        WORKER_RESULT.store(-1, SeqCst);
    }
    0
}

#[test_harness::test]
fn send_recv_round_trip_receiver_first() {
    let ep = Port::create().expect("port create");
    EP_RAW.store(u64::from(ep.as_raw().raw()), SeqCst);

    let worker = spawn_worker(receiver_worker);
    while WORKER_READY.load(SeqCst) == 0 {
        core::hint::spin_loop();
    }

    ep.send_bytes(b"hello", Timeout::INFINITE).expect("send");

    join(worker);
    test_harness::check_eq!(WORKER_RESULT.load(SeqCst), 0);
}

// --- (b) call/reply round-trip: worker = server (Port::recv + Reply::reply), -
//        main = caller (raw call на тот же id).

fn serve_one_call(ep: &Port) -> u32 {
    WORKER_READY.store(1, SeqCst);
    let mut buf = [0u8; 8];
    let Ok((len, Some(reply))) = ep.recv_bytes(&mut buf, Timeout::INFINITE) else {
        WORKER_RESULT.store(-100, SeqCst);
        return 0;
    };
    if len != 3 || &buf[..3] != b"req" {
        WORKER_RESULT.store(-1, SeqCst);
        return 0;
    }
    let rr = reply.reply_bytes(b"resp");
    WORKER_RESULT.store(if rr.is_ok() { 0 } else { -2 }, SeqCst);
    0
}

#[test_harness::test]
fn call_reply_round_trip() {
    let ep = Port::create().expect("port create");
    let ep_raw = ep.as_raw();

    WORKER_RESULT.store(i64::MIN, SeqCst);
    WORKER_READY.store(0, SeqCst);
    let worker = spawn_owned(move || serve_one_call(&ep)).expect("spawn worker");
    while WORKER_READY.load(SeqCst) == 0 {
        core::hint::spin_loop();
    }

    let va = ipc_va();
    // SAFETY: собственный буфер главного потока.
    unsafe {
        write_msg(va, b"req", &[]);
    }
    test_harness::check_eq!(port_call(ep_raw, Timeout::INFINITE), 0);

    let mut buf = [0u8; 8];
    // SAFETY: собственный буфер.
    let (len, ncaps) = unsafe { read_msg(va, &mut buf) };
    test_harness::check_eq!(len, 4);
    test_harness::check_eq!(ncaps, 0);
    test_harness::check!(&buf[..4] == b"resp");

    join(worker);
    test_harness::check_eq!(WORKER_RESULT.load(SeqCst), 0);
}

// --- (c) cap transfer: worker = sender (передаёт Signal), main = recv. -

extern "C" fn cap_sender_worker(_arg: usize) -> u32 {
    let va = ipc_va();
    // Создаём Signal и поднимаем сигнал до переноса. Бит должен
    // "уехать" вместе с объектом и быть видим получателю - это доказывает
    // идентичность capability target (свежий объект бита бы не имел).
    let notif = signal_create().expect("signal_create");
    NOTIF_RAW.store(u64::from(notif.as_raw().raw()), SeqCst);
    if signal_set(notif.as_raw(), SIGNALED, 0, WakeCount::None) != 0 {
        WORKER_RESULT.store(-100, SeqCst);
        return 0;
    }
    // SAFETY: собственный буфер; передаём 0-байтовое тело и 1 cap.
    unsafe {
        write_msg(va, b"", &[notif.as_raw().raw()]);
    }
    WORKER_READY.store(1, SeqCst);
    let r = port_send(ep_handle(), Timeout::INFINITE);
    if r == 0 {
        let _ = notif.leak();
    }
    WORKER_RESULT.store(r, SeqCst);
    0
}

#[test_harness::test]
fn cap_transfer_through_port() {
    let ep = Port::create().expect("port create");
    EP_RAW.store(u64::from(ep.as_raw().raw()), SeqCst);

    let worker = spawn_worker(cap_sender_worker);
    while WORKER_READY.load(SeqCst) == 0 {
        core::hint::spin_loop();
    }

    let va = ipc_va();
    let reply = ep.recv(Timeout::INFINITE).expect("recv");
    test_harness::check!(reply.is_none());

    let mut buf = [0u8; 1];
    // SAFETY: собственный буфер главного потока.
    let (len, ncaps) = unsafe { read_msg(va, &mut buf) };
    test_harness::check_eq!(len, 0);
    test_harness::check_eq!(ncaps, 1);

    // SAFETY: собственный буфер.
    let received_raw = unsafe { read_cap(va, 0) };
    let received = NonZeroU32::new(received_raw)
        .map(Handle::<kind::Signal>::from_abi)
        .expect("received cap handle");

    let observed = signal_wait_one(received, SIGNALED, Timeout::POLL);
    let bit = i64::from(SIGNALED);
    test_harness::check!(observed & bit == bit);

    join(worker);
    test_harness::check_eq!(WORKER_RESULT.load(SeqCst), 0);
}

// --- (e) badge round-trip: сервер минтит две badged-копии, два клиента ---
//        шлют через свои копии, сервер получает разные badge.

const BADGE_A: u64 = 0xA11CE;
const BADGE_B: u64 = 0xB0B;

static EP_BADGED: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];
static CLIENTS_READY: AtomicU64 = AtomicU64::new(0);
static CLIENT_RESULTS: [AtomicI64; 2] = [const { AtomicI64::new(i64::MIN) }; 2];

extern "C" fn badge_client(index: usize) -> u32 {
    let va = ipc_va();
    let h = port_of(EP_BADGED[index].load(SeqCst) as u32).expect("badged copy");
    // SAFETY: собственный буфер.
    unsafe {
        write_msg(va, &[b"ab"[index]], &[]);
    }
    CLIENTS_READY.fetch_add(1, SeqCst);
    let r = port_send(h, Timeout::INFINITE);
    CLIENT_RESULTS[index].store(r, SeqCst);
    0
}

#[test_harness::test]
fn badge_round_trip_two_clients() {
    let ep = Port::create().expect("port create");
    let ep_raw = ep.as_raw();

    let copy_a = handle_duplicate(ep_raw, RIGHTS_WRITE, BADGE_A).expect("mint badged copy A");
    let copy_b = handle_duplicate(ep_raw, RIGHTS_WRITE, BADGE_B).expect("mint badged copy B");
    EP_BADGED[0].store(u64::from(copy_a.as_raw().raw()), SeqCst);
    EP_BADGED[1].store(u64::from(copy_b.as_raw().raw()), SeqCst);
    CLIENTS_READY.store(0, SeqCst);
    for result in &CLIENT_RESULTS {
        result.store(i64::MIN, SeqCst);
    }

    let client_a = spawn(badge_client, 0).expect("spawn client A");
    let client_b = spawn(badge_client, 1).expect("spawn client B");

    while CLIENTS_READY.load(SeqCst) < 2 {
        core::hint::spin_loop();
    }

    let va = ipc_va();
    let mut seen = [0u64; 2];
    for slot in &mut seen {
        let reply = ep.recv(Timeout::INFINITE).expect("recv");
        test_harness::check!(reply.is_none());
        // SAFETY: собственный буфер сервера.
        *slot = unsafe { read_badge(va) };
    }

    let got_a = seen[0] == BADGE_A || seen[1] == BADGE_A;
    let got_b = seen[0] == BADGE_B || seen[1] == BADGE_B;
    test_harness::check!(got_a);
    test_harness::check!(got_b);
    test_harness::check!(seen[0] != seen[1]);

    join(client_a);
    join(client_b);
    for result in &CLIENT_RESULTS {
        test_harness::check_eq!(result.load(SeqCst), 0);
    }
}

// --- (f) timeout/poll: блокирующие Port-syscall не виснут на RT-пути. ---------

#[test_harness::test]
fn send_poll_no_receiver_should_wait() {
    let ep = Port::create().expect("port create");
    assert_timeout(ep.send_bytes(b"x", Timeout::POLL));
}

#[test_harness::test]
fn recv_poll_no_sender_should_wait() {
    let ep = Port::create().expect("port create");
    assert_timeout(ep.recv(Timeout::POLL).map(|_| ()));
}

#[test_harness::test]
fn recv_finite_timeout_no_sender_should_wait() {
    let ep = Port::create().expect("port create");
    assert_timeout(ep.recv(Timeout::from_millis(10)).map(|_| ()));
}

#[test_harness::test]
fn call_finite_timeout_no_server_should_wait() {
    let ep = Port::create().expect("port create");
    let mut buf = [0u8; 8];
    assert_timeout(
        ep.call_bytes(b"req", &mut buf, Timeout::from_millis(10))
            .map(|_| ()),
    );
}

fn assert_timeout(result: kernel_objects::Result<()>) {
    test_harness::check!(matches!(result, Err(Error::Syscall(SyscallError::Timeout))));
}

// --- (g) Порт ждут через `wait_any`, а кадр готового порта принимают опросом. ---------------

const PROBE: &[u8] = b"probe";

const READY_TIMEOUT: Timeout = Timeout::from_secs(5);
const SENDER_TIMEOUT: Timeout = Timeout::from_millis(200);
const IDLE_TIMEOUT: Timeout = Timeout::from_millis(50);

fn send_probe(wait: Timeout) {
    crate::ipc_raw::write_frame(PROBE, &[]);
    WORKER_RESULT.store(port_send(ep_handle(), wait), SeqCst);
}

extern "C" fn probe_sender(_arg: usize) -> u32 {
    send_probe(READY_TIMEOUT);
    0
}

extern "C" fn expiring_sender(_arg: usize) -> u32 {
    send_probe(SENDER_TIMEOUT);
    0
}

extern "C" fn polling_sender(_arg: usize) -> u32 {
    send_probe(Timeout::POLL);
    0
}

#[test_harness::test]
fn waiting_port_and_signal_wakes_on_a_sender() {
    let ep = Port::create().expect("port create");
    let signal = Signal::create().expect("signal create");
    EP_RAW.store(u64::from(ep.as_raw().raw()), SeqCst);

    let worker = spawn_worker(probe_sender);
    let items = [
        WaitItem::new(signal.as_raw(), SIGNALED),
        WaitItem::new(ep.as_raw(), READABLE),
    ];
    test_harness::check!(matches!(wait_any(&items, READY_TIMEOUT), Ok(1)));

    let mut buf = [0u8; 8];
    let received = ep.recv_bytes(&mut buf, Timeout::POLL);
    test_harness::check!(matches!(received, Ok((len, None)) if buf.get(..len) == Some(PROBE)));

    join(worker);
    test_harness::check_eq!(WORKER_RESULT.load(SeqCst), 0);
}

#[test_harness::test]
fn waiting_port_times_out_after_its_sender_expires() {
    let ep = Port::create().expect("port create");
    EP_RAW.store(u64::from(ep.as_raw().raw()), SeqCst);

    let worker = spawn_worker(expiring_sender);
    let items = [WaitItem::new(ep.as_raw(), READABLE)];
    test_harness::check!(matches!(wait_any(&items, READY_TIMEOUT), Ok(0)));

    join(worker);
    test_harness::check_eq!(
        WORKER_RESULT.load(SeqCst),
        SyscallError::Timeout.as_return_value()
    );
    assert_timeout(wait_any(&items, IDLE_TIMEOUT).map(|_| ()));
}

#[test_harness::test]
fn waiting_port_times_out_while_a_client_polls_send() {
    let ep = Port::create().expect("port create");
    EP_RAW.store(u64::from(ep.as_raw().raw()), SeqCst);

    let worker = spawn_worker(polling_sender);
    let items = [WaitItem::new(ep.as_raw(), READABLE)];
    assert_timeout(wait_any(&items, IDLE_TIMEOUT).map(|_| ()));

    join(worker);
    test_harness::check_eq!(
        WORKER_RESULT.load(SeqCst),
        SyscallError::Timeout.as_return_value()
    );
}
