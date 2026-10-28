//! E2E завершения из EL0: `ThreadExit(N)` помечает объект завершённым и
//! публикует `N` как exit-код - и для потока текущего процесса, и для
//! процесса-потомка целиком. Уход последнего держателя capability снимает
//! процесс, а вместе с ним процессы, которые снятый держал. Снятие процесса и
//! `ThreadTerminate` отпускают ресурсы потоков, ждущих в syscall.

use kernel_objects::{Error, Port, Process, Resource, Thread, ThreadEntry};
use runtime::{
    Priority, Timeout, UserMemFlags, signal_wait_one, spawn_owned, thread_exit, thread_exit_code,
    thread_terminate,
};
use syscall::{Rights, SIGNALED, SyscallError};

use crate::{
    fixture::{Fixture, JOIN_TIMEOUT},
    fixture_protocol::{
        OP_CALL_SIBLING, OP_CLOSE_CAP, OP_EXIT, OP_HOLD_CAP, OP_HOLD_SELF, OP_PROCESS_EXIT,
        OP_RECV_SIBLING, OP_SPIN_SIBLING, OP_WRITE_SLOT, REPLY_OK, encode_request,
    },
    ipc_raw,
    system_info_session::{settled_snapshot, system_info},
};

const STACK_SIZE: u64 = 0x4000;
const EXIT_CODE: u32 = 42;
const PROCESS_EXIT_CODE: u32 = 77;
const REMOVAL_TIMEOUT: Timeout = Timeout::from_secs(5);
const LIFETIME_PROBE: u64 = 0xA11FE;
const TERMINATE_EXIT_CODE: u32 = 55;
const RELAUNCHES: usize = 8;

fn terminate(fixture: &Fixture) {
    fixture
        .process()
        .terminate(TERMINATE_EXIT_CODE)
        .expect("terminate the fixture");
    fixture
        .process()
        .join(JOIN_TIMEOUT)
        .expect("terminated fixture finishes");
    test_harness::check_eq!(
        fixture.process().exit_code().expect("fixture exit code"),
        TERMINATE_EXIT_CODE
    );
}

fn fixture_with_receiving_sibling() -> Fixture {
    let fixture = Fixture::spawn();
    test_harness::check_eq!(fixture.request_status(OP_RECV_SIBLING, 0, &[]), REPLY_OK);
    fixture
}

fn assert_fixture_removed(port: &Port) {
    ipc_raw::write_frame(&encode_request(OP_WRITE_SLOT, LIFETIME_PROBE), &[]);

    // До дренажа таблицы снятого процесса запрос паркуется и получает `PeerClosed` на
    // обнулении множества получателей, после дренажа отказ приходит отзывом деривации.
    test_harness::check!(matches!(
        port.call(REMOVAL_TIMEOUT).err(),
        Some(Error::Syscall(
            SyscallError::Revoked | SyscallError::PeerClosed
        ))
    ));
}

extern "C" fn exiting_entry(code: usize) -> ! {
    thread_exit(code as u64)
}

#[test_harness::test]
fn thread_exit_publishes_exit_code() {
    let stack = Resource::self_resource()
        .expect("self metering resource")
        .allocate(STACK_SIZE, UserMemFlags::ReadWrite)
        .expect("allocate worker stack");
    let process = Process::self_process().expect("self process handle");

    let entry: extern "C" fn(usize) -> ! = exiting_entry;
    let thread = Thread::create(
        process.borrow(),
        ThreadEntry {
            entry_pc: entry as *const () as u64,
            user_sp: stack.va() + STACK_SIZE,
            arg: u64::from(EXIT_CODE),
            priority: Priority::new(1),
        },
    )
    .expect("create worker thread");

    thread
        .join(JOIN_TIMEOUT)
        .expect("worker terminates before timeout");
    test_harness::check_eq!(thread.exit_code().expect("thread exit code"), EXIT_CODE);
}

#[test_harness::test]
fn child_process_exit_code_is_observable() {
    let fixture = Fixture::spawn();

    let status = fixture.request_status(OP_EXIT, u64::from(EXIT_CODE), &[]);
    test_harness::check_eq!(status, REPLY_OK);

    fixture
        .process()
        .join(JOIN_TIMEOUT)
        .expect("child terminates");
    test_harness::check_eq!(
        fixture.process().exit_code().expect("child exit code"),
        EXIT_CODE
    );
}

#[test_harness::test]
fn process_exit_terminates_the_parked_sibling() {
    let fixture = Fixture::spawn();

    let status = fixture.request_status(OP_PROCESS_EXIT, u64::from(PROCESS_EXIT_CODE), &[]);
    test_harness::check_eq!(status, REPLY_OK);

    fixture
        .process()
        .join(JOIN_TIMEOUT)
        .expect("child terminates");
    test_harness::check_eq!(
        fixture.process().exit_code().expect("child exit code"),
        PROCESS_EXIT_CODE
    );
}

#[test_harness::test]
fn closing_the_last_child_capability_removes_the_child() {
    let fixture = Fixture::spawn();

    test_harness::check_eq!(
        fixture.request_status(OP_WRITE_SLOT, LIFETIME_PROBE, &[]),
        REPLY_OK
    );

    let (process, port) = fixture.into_parts();
    drop(process);

    assert_fixture_removed(&port);
}

#[test_harness::test]
fn removing_a_holder_removes_the_processes_it_held() {
    let holder = Fixture::spawn();
    let (child_process, child_port) = Fixture::spawn().into_parts();

    let child_cap = child_process.into_raw().raw();
    test_harness::check_eq!(
        holder.request_status(OP_HOLD_CAP, 0, &[child_cap]),
        REPLY_OK
    );

    let (holder_process, holder_port) = holder.into_parts();
    drop(holder_process);

    assert_fixture_removed(&holder_port);
    assert_fixture_removed(&child_port);
}

#[test_harness::test]
fn closing_the_last_holder_from_inside_leaves_the_request_unanswered() {
    let (process, port) = Fixture::spawn().into_parts();

    let own_cap = process.into_raw().raw();
    ipc_raw::write_frame(&encode_request(OP_CLOSE_CAP, 0), &[own_cap]);

    test_harness::check!(matches!(
        port.call(REMOVAL_TIMEOUT).err(),
        Some(Error::Syscall(SyscallError::PeerClosed))
    ));
    assert_fixture_removed(&port);
}

#[test_harness::test]
fn a_self_capability_does_not_keep_the_child_alive() {
    let fixture = Fixture::spawn();
    test_harness::check_eq!(fixture.request_status(OP_HOLD_SELF, 0, &[]), REPLY_OK);

    let (process, port) = fixture.into_parts();
    drop(process);

    assert_fixture_removed(&port);
}

#[test_harness::test]
fn terminate_takes_down_a_sibling_waiting_in_recv() {
    terminate(&fixture_with_receiving_sibling());
}

#[test_harness::test]
fn terminate_takes_down_a_sibling_waiting_for_a_reply() {
    let server = Port::create().expect("server port");
    let client = server
        .duplicate(Rights::WRITE | Rights::TRANSFER, None)
        .expect("client handle for the fixture");
    let fixture = Fixture::spawn();
    test_harness::check_eq!(
        fixture.request_status(OP_CALL_SIBLING, 0, &[client.into_raw().raw()]),
        REPLY_OK
    );
    let reply = server
        .recv(JOIN_TIMEOUT)
        .expect("the sibling call arrives")
        .expect("a call carries a reply");

    terminate(&fixture);

    // Ожидание ответа снято отменой, и поздний ответ некому доставить.
    ipc_raw::write_frame(&[], &[]);
    test_harness::check!(matches!(
        reply.reply().err(),
        Some(Error::Syscall(SyscallError::PeerClosed))
    ));
}

#[test_harness::test]
fn thread_terminate_takes_down_a_thread_waiting_for_a_reply() {
    let server = Port::create().expect("server port");
    let client = server
        .duplicate(Rights::WRITE, None)
        .expect("client handle for the worker");
    let worker = spawn_owned(move || {
        ipc_raw::write_frame(&[], &[]);
        let _ = client.call(Timeout::INFINITE);
        0
    })
    .expect("spawn worker");
    let reply = server
        .recv(JOIN_TIMEOUT)
        .expect("the worker call arrives")
        .expect("a call carries a reply");

    test_harness::check_eq!(
        thread_terminate(worker.handle(), u64::from(TERMINATE_EXIT_CODE)),
        0
    );
    test_harness::check!(signal_wait_one(worker.handle(), SIGNALED, JOIN_TIMEOUT) >= 0);
    test_harness::check_eq!(
        thread_exit_code(worker.handle()),
        i64::from(TERMINATE_EXIT_CODE)
    );
    worker
        .join(JOIN_TIMEOUT)
        .expect("a terminated worker joins");

    ipc_raw::write_frame(&[], &[]);
    test_harness::check!(matches!(
        reply.reply().err(),
        Some(Error::Syscall(SyscallError::PeerClosed))
    ));
}

#[test_harness::test]
fn terminate_takes_down_a_sibling_spinning_in_el0() {
    let fixture = Fixture::spawn();
    test_harness::check_eq!(fixture.request_status(OP_SPIN_SIBLING, 0, &[]), REPLY_OK);

    terminate(&fixture);
}

#[test_harness::test]
fn relaunching_a_process_with_a_waiting_thread_returns_its_frames() {
    let system_info = system_info();
    // Первый запуск прогревает кучи testrunner и ядра.
    terminate(&fixture_with_receiving_sibling());
    let before = settled_snapshot(&system_info).free_frames;

    for _ in 0..RELAUNCHES {
        terminate(&fixture_with_receiving_sibling());
    }

    let after = settled_snapshot(&system_info).free_frames;
    test_harness::check_eq!(after, before);
}
