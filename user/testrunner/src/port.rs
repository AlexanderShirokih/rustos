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
//!   объекта на стороне получателя (сигнал, поднятый ДО переноса, виден);
//! - (d) порядок «получатель пришёл первым» и «отправитель пришёл первым».

use core::sync::atomic::{AtomicI64, AtomicU64, Ordering::SeqCst};

use kernel_tests::kernel_test;
use runtime::{
    Error, OwnedHandle, Port, Timeout, handle_duplicate, ipc_buffer_addr, memory_allocate,
    port_call, port_create, port_recv, port_send, process_resource_self, process_self,
    signal_create, signal_set, signal_wait_one, thread_create, thread_exit,
};
use syscall::{
    Handle, MEM_FLAGS_READ_WRITE, SIGNALED, SyscallError, WakeCount, decode_tag, encode_tag,
};

const STACK_SIZE: u64 = 0x4000;
const JOIN_TIMEOUT_NS: u64 = 5_000_000_000;

/// Офсеты полей IpcBuffer (`#[repr(C)]`): tag@0, caps@8, data@24, badge@280.
const CAPS_OFF: usize = 8;
const DATA_OFF: usize = 24;
const BADGE_OFF: usize = 280;

/// Право `Rights::WRITE` (бит `1<<3`) для минта badged-копии port'а,
/// через которую клиент может слать (`send` гейтится WRITE).
const RIGHTS_WRITE: u32 = 1 << 3;

/// Сырой HandleId port'а, передаваемый worker-потоку (он не может
/// принять аргумент handle напрямую кроме x0).
static EP_RAW: AtomicU64 = AtomicU64::new(0);
/// Результат worker-потока (>=0 - ok-код, <0 - ошибка/маркер).
static WORKER_RESULT: AtomicI64 = AtomicI64::new(i64::MIN);
/// Готовность worker'а (для биаса порядка прихода).
static WORKER_READY: AtomicU64 = AtomicU64::new(0);
/// Сырой HandleId Signal, который worker создаёт для cap-transfer теста.
static NOTIF_RAW: AtomicU64 = AtomicU64::new(0);

fn ipc_va() -> usize {
    let ret = ipc_buffer_addr();
    assert_positive(ret);
    usize::try_from(ret).expect("positive va fits usize")
}

fn assert_positive(v: i64) {
    kernel_tests::kassert!(v > 0);
}

/// Пишет tag + тело в собственный IPC-буфер.
///
/// SAFETY: ядро замаппило per-thread IPC-буфер user-RW (одна страница);
/// запись tag/data лежит в его границах, поток - единственный владелец.
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

/// Читает (len, ncaps) и первые `out.len()` байт тела из собственного буфера.
///
/// SAFETY: см. [`write_msg`].
unsafe fn read_msg(va: usize, out: &mut [u8]) -> (usize, usize) {
    // SAFETY: caller guarantees `va` points to the current thread's per-thread IPC-буфер.
    unsafe {
        let tag = (va as *const u64).read_volatile();
        let (len, ncaps) = decode_tag(tag);
        let data = (va + DATA_OFF) as *const u8;
        let n = out.len().min(len);
        for (i, slot) in out.iter_mut().enumerate().take(n) {
            *slot = data.add(i).read_volatile();
        }
        (len, ncaps)
    }
}

/// Читает cap[i] из собственного буфера.
///
/// SAFETY: см. [`write_msg`].
unsafe fn read_cap(va: usize, i: usize) -> u32 {
    // SAFETY: caller guarantees `va` points to the current thread's per-thread IPC-буфер.
    unsafe { ((va + CAPS_OFF) as *const u32).add(i).read_volatile() }
}

/// Читает значок (badge) отправителя из собственного буфера (ядро пишет его
/// туда на recv/call).
///
/// SAFETY: см. [`write_msg`].
unsafe fn read_badge(va: usize) -> u64 {
    // SAFETY: caller guarantees `va` points to the current thread's per-thread IPC-буфер.
    unsafe { ((va + BADGE_OFF) as *const u64).read_volatile() }
}

fn alloc_stack() -> u64 {
    let resource = process_resource_self().expect("metering resource handle");
    let stack = memory_allocate(resource, STACK_SIZE, MEM_FLAGS_READ_WRITE);
    assert_positive(stack);
    u64::try_from(stack).expect("positive va fits u64") + STACK_SIZE
}

fn ep_handle() -> Handle {
    Handle::new(EP_RAW.load(SeqCst) as u32).expect("port handle present")
}

fn spawn_worker(entry: extern "C" fn(usize) -> !) -> Handle {
    WORKER_RESULT.store(i64::MIN, SeqCst);
    WORKER_READY.store(0, SeqCst);
    let sp = alloc_stack();
    let process = process_self().expect("process_self");
    let e = entry as *const () as u64;
    thread_create(process, e, sp, 0, 1).expect("thread_create")
}

fn join(thread: Handle) {
    let observed = signal_wait_one(thread, SIGNALED, JOIN_TIMEOUT_NS);
    kernel_tests::kassert_eq!(observed, i64::from(SIGNALED));
}

// --- (a) + (d, sender-first): worker = sender, main = receiver. --------------

extern "C" fn sender_worker(_arg: usize) -> ! {
    let va = ipc_va();
    // SAFETY: собственный per-thread буфер.
    unsafe {
        write_msg(va, b"ping", &[]);
    }
    WORKER_READY.store(1, SeqCst);
    let r = port_send(ep_handle(), Timeout::INFINITE.raw());
    WORKER_RESULT.store(r, SeqCst);
    thread_exit(0)
}

#[kernel_test]
fn send_recv_round_trip_sender_first() {
    let ep = Port::create().expect("port create");
    EP_RAW.store(u64::from(ep.handle().as_raw().raw()), SeqCst);

    let worker = spawn_worker(sender_worker);
    // Биас: ждём, пока worker объявит готовность (он блокируется в send сразу
    // после). Корректность не зависит от порядка - recv смэтчит припаркованного
    // отправителя либо припаркуется сам.
    while WORKER_READY.load(SeqCst) == 0 {
        core::hint::spin_loop();
    }

    let mut buf = [0u8; 8];
    let (len, reply) = ep.recv_bytes(&mut buf, Timeout::INFINITE).expect("recv");
    kernel_tests::kassert!(reply.is_none()); // не call -> нет reply
    kernel_tests::kassert_eq!(len, 4);
    kernel_tests::kassert!(&buf[..4] == b"ping");

    join(worker);
    kernel_tests::kassert_eq!(WORKER_RESULT.load(SeqCst), 0);
}

// --- (d, receiver-first): worker = receiver, main = sender. ------------------

extern "C" fn receiver_worker(_arg: usize) -> ! {
    let va = ipc_va();
    WORKER_READY.store(1, SeqCst);
    let r = port_recv(ep_handle(), Timeout::INFINITE.raw());
    if r != 0 {
        WORKER_RESULT.store(-100, SeqCst);
        thread_exit(0);
    }
    let mut buf = [0u8; 8];
    // SAFETY: собственный буфер worker-потока.
    let (len, _ncaps) = unsafe { read_msg(va, &mut buf) };
    if len == 5 && &buf[..5] == b"hello" {
        WORKER_RESULT.store(0, SeqCst);
    } else {
        WORKER_RESULT.store(-1, SeqCst);
    }
    thread_exit(0)
}

#[kernel_test]
fn send_recv_round_trip_receiver_first() {
    let ep = Port::create().expect("port create");
    EP_RAW.store(u64::from(ep.handle().as_raw().raw()), SeqCst);

    let worker = spawn_worker(receiver_worker);
    while WORKER_READY.load(SeqCst) == 0 {
        core::hint::spin_loop();
    }

    ep.send_bytes(b"hello", Timeout::INFINITE).expect("send");

    join(worker);
    kernel_tests::kassert_eq!(WORKER_RESULT.load(SeqCst), 0);
}

// --- (b) call/reply round-trip: worker = server (Port::recv + Reply::reply), -
//        main = caller (raw call на тот же id).

extern "C" fn server_worker(_arg: usize) -> ! {
    // Сервер - единственный владелец Port (усыновляет id, закроет на drop);
    // main оперирует тем же id сырым call и не закрывает.
    // SAFETY: единственный OwnedHandle-владелец port'а - этот worker (main
    // оперирует тем же id сырым call, обёртку не создаёт).
    let ep = Port::from_handle(
        unsafe { OwnedHandle::from_raw(EP_RAW.load(SeqCst) as u32) }.expect("port id"),
    );
    WORKER_READY.store(1, SeqCst);
    let mut buf = [0u8; 8];
    let Ok((len, Some(reply))) = ep.recv_bytes(&mut buf, Timeout::INFINITE) else {
        WORKER_RESULT.store(-100, SeqCst);
        thread_exit(0);
    };
    if len != 3 || &buf[..3] != b"req" {
        WORKER_RESULT.store(-1, SeqCst);
        thread_exit(0);
    }
    let rr = reply.reply_bytes(b"resp");
    WORKER_RESULT.store(if rr.is_ok() { 0 } else { -2 }, SeqCst);
    thread_exit(0)
}

#[kernel_test]
fn call_reply_round_trip() {
    let ep = port_create().expect("port_create");
    EP_RAW.store(u64::from(ep.raw()), SeqCst);

    let worker = spawn_worker(server_worker);
    while WORKER_READY.load(SeqCst) == 0 {
        core::hint::spin_loop();
    }

    let va = ipc_va();
    // SAFETY: собственный буфер главного потока.
    unsafe {
        write_msg(va, b"req", &[]);
    }
    kernel_tests::kassert_eq!(port_call(ep, Timeout::INFINITE.raw()), 0);

    let mut buf = [0u8; 8];
    // SAFETY: собственный буфер.
    let (len, ncaps) = unsafe { read_msg(va, &mut buf) };
    kernel_tests::kassert_eq!(len, 4);
    kernel_tests::kassert_eq!(ncaps, 0);
    kernel_tests::kassert!(&buf[..4] == b"resp");

    join(worker);
    kernel_tests::kassert_eq!(WORKER_RESULT.load(SeqCst), 0);
}

// --- (c) cap transfer: worker = sender (передаёт Signal), main = recv. -

extern "C" fn cap_sender_worker(_arg: usize) -> ! {
    let va = ipc_va();
    // Создаём Signal и поднимаем сигнал ДО переноса: бит должен
    // «уехать» вместе с объектом и быть видим получателю - это доказывает
    // идентичность capability target (свежий объект бита бы не имел).
    let notif = signal_create().expect("signal_create");
    NOTIF_RAW.store(u64::from(notif.raw()), SeqCst);
    if signal_set(notif, SIGNALED, 0, WakeCount::None) != 0 {
        WORKER_RESULT.store(-100, SeqCst);
        thread_exit(0);
    }
    // SAFETY: собственный буфер; передаём 0-байтовое тело и 1 cap.
    unsafe {
        write_msg(va, b"", &[notif.raw()]);
    }
    WORKER_READY.store(1, SeqCst);
    let r = port_send(ep_handle(), Timeout::INFINITE.raw());
    WORKER_RESULT.store(r, SeqCst);
    thread_exit(0)
}

#[kernel_test]
fn cap_transfer_through_port() {
    let ep = Port::create().expect("port create");
    EP_RAW.store(u64::from(ep.handle().as_raw().raw()), SeqCst);

    let worker = spawn_worker(cap_sender_worker);
    while WORKER_READY.load(SeqCst) == 0 {
        core::hint::spin_loop();
    }

    let va = ipc_va();
    let reply = ep.recv(Timeout::INFINITE).expect("recv");
    kernel_tests::kassert!(reply.is_none());

    let mut buf = [0u8; 1];
    // SAFETY: собственный буфер главного потока.
    let (len, ncaps) = unsafe { read_msg(va, &mut buf) };
    kernel_tests::kassert_eq!(len, 0);
    kernel_tests::kassert_eq!(ncaps, 1);

    // Получили новый HandleId на ту же Signal.
    // SAFETY: собственный буфер.
    let received_raw = unsafe { read_cap(va, 0) };
    let received = Handle::new(received_raw).expect("received cap handle");

    // Идентичность: бит, поднятый отправителем ДО переноса, виден здесь.
    let observed = signal_wait_one(received, SIGNALED, 0);
    let bit = i64::from(SIGNALED);
    kernel_tests::kassert!(observed & bit == bit);

    join(worker);
    kernel_tests::kassert_eq!(WORKER_RESULT.load(SeqCst), 0);
}

// --- (e) badge round-trip: сервер минтит две badged-копии, два клиента ---
//        шлют через свои копии, сервер видит РАЗНЫЕ badge.

const BADGE_A: u64 = 0xA11CE;
const BADGE_B: u64 = 0xB0B;

/// Сырые HandleId badged-копий port'а для клиентов A и B.
static EP_BADGED_A: AtomicU64 = AtomicU64::new(0);
static EP_BADGED_B: AtomicU64 = AtomicU64::new(0);
/// Готовность клиентов (оба должны заклеймиться и быть готовы слать).
static CLIENTS_READY: AtomicU64 = AtomicU64::new(0);
/// Результаты клиентских send (>=0 ok).
static CLIENT_A_RESULT: AtomicI64 = AtomicI64::new(i64::MIN);
static CLIENT_B_RESULT: AtomicI64 = AtomicI64::new(i64::MIN);

extern "C" fn badge_client_a(_arg: usize) -> ! {
    let va = ipc_va();
    let h = Handle::new(EP_BADGED_A.load(SeqCst) as u32).expect("badged copy A");
    // SAFETY: собственный буфер.
    unsafe {
        write_msg(va, b"a", &[]);
    }
    CLIENTS_READY.fetch_add(1, SeqCst);
    let r = port_send(h, Timeout::INFINITE.raw());
    CLIENT_A_RESULT.store(r, SeqCst);
    thread_exit(0)
}

extern "C" fn badge_client_b(_arg: usize) -> ! {
    let va = ipc_va();
    let h = Handle::new(EP_BADGED_B.load(SeqCst) as u32).expect("badged copy B");
    // SAFETY: собственный буфер.
    unsafe {
        write_msg(va, b"b", &[]);
    }
    CLIENTS_READY.fetch_add(1, SeqCst);
    let r = port_send(h, Timeout::INFINITE.raw());
    CLIENT_B_RESULT.store(r, SeqCst);
    thread_exit(0)
}

#[kernel_test]
fn badge_round_trip_two_clients() {
    let ep = Port::create().expect("port create");
    let ep_raw = ep.handle().as_raw();

    // Минтим две badged-копии (set-once: оригинал незаклеймён -> копии несут
    // заданный badge). Права сужаем до WRITE - клиенту нужен только send.
    let copy_a = handle_duplicate(ep_raw, RIGHTS_WRITE, BADGE_A).expect("mint badged copy A");
    let copy_b = handle_duplicate(ep_raw, RIGHTS_WRITE, BADGE_B).expect("mint badged copy B");
    EP_BADGED_A.store(u64::from(copy_a.raw()), SeqCst);
    EP_BADGED_B.store(u64::from(copy_b.raw()), SeqCst);
    CLIENTS_READY.store(0, SeqCst);
    CLIENT_A_RESULT.store(i64::MIN, SeqCst);
    CLIENT_B_RESULT.store(i64::MIN, SeqCst);

    let process = process_self().expect("process_self");
    let sp_a = alloc_stack();
    let sp_b = alloc_stack();
    let client_a = thread_create(process, badge_client_a as *const () as u64, sp_a, 0, 1)
        .expect("thread_create A");
    let client_b = thread_create(process, badge_client_b as *const () as u64, sp_b, 0, 1)
        .expect("thread_create B");

    while CLIENTS_READY.load(SeqCst) < 2 {
        core::hint::spin_loop();
    }

    // Сервер принимает два сообщения и собирает наблюдённые значки. Порядок
    // прихода не детерминирован - проверяем как множество.
    let va = ipc_va();
    let mut seen = [0u64; 2];
    for slot in &mut seen {
        let reply = ep.recv(Timeout::INFINITE).expect("recv");
        kernel_tests::kassert!(reply.is_none()); // не call -> нет reply
        // SAFETY: собственный буфер сервера.
        *slot = unsafe { read_badge(va) };
    }

    // Оба значка получены и различны, совпадают с выданными.
    let got_a = seen[0] == BADGE_A || seen[1] == BADGE_A;
    let got_b = seen[0] == BADGE_B || seen[1] == BADGE_B;
    kernel_tests::kassert!(got_a);
    kernel_tests::kassert!(got_b);
    kernel_tests::kassert!(seen[0] != seen[1]);

    join(client_a);
    join(client_b);
    kernel_tests::kassert_eq!(CLIENT_A_RESULT.load(SeqCst), 0);
    kernel_tests::kassert_eq!(CLIENT_B_RESULT.load(SeqCst), 0);
}

// --- (f) timeout/poll: блокирующие Port-syscall'ы не виснут на RT-пути. -------

#[kernel_test]
fn send_poll_no_receiver_should_wait() {
    let ep = Port::create().expect("port create");
    assert_timeout(ep.send_bytes(b"x", Timeout::POLL));
}

#[kernel_test]
fn recv_poll_no_sender_should_wait() {
    let ep = Port::create().expect("port create");
    assert_timeout(ep.recv(Timeout::POLL).map(|_| ()));
}

#[kernel_test]
fn recv_finite_timeout_no_sender_should_wait() {
    let ep = Port::create().expect("port create");
    // 10 мс - заведомо истечёт, отправителя нет.
    assert_timeout(ep.recv(Timeout::from_ns(10_000_000)).map(|_| ()));
}

#[kernel_test]
fn call_finite_timeout_no_server_should_wait() {
    let ep = Port::create().expect("port create");
    let mut buf = [0u8; 8];
    assert_timeout(
        ep.call_bytes(b"req", &mut buf, Timeout::from_ns(10_000_000))
            .map(|_| ()),
    );
}

/// Падает, если результат не `Err(SyscallError::Timeout)`.
fn assert_timeout(result: runtime::Result<()>) {
    kernel_tests::kassert!(matches!(result, Err(Error::Syscall(SyscallError::Timeout))));
}
