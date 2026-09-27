//! E2E завершения из EL0: `ThreadExit(N)` помечает объект завершённым и
//! публикует `N` как exit-код - и для потока текущего процесса, и для
//! процесса-потомка целиком. Уход последнего держателя capability снимает
//! процесс, а вместе с ним процессы, которые снятый держал.

use kernel_objects::{Error, Port, Process, Resource, Thread, ThreadEntry};
use kernel_tests::kernel_test;
use runtime::{Priority, Timeout, UserMemFlags, thread_exit};
use syscall::SyscallError;

use crate::{
    fixture::{Fixture, JOIN_TIMEOUT},
    fixture_protocol::{
        OP_CLOSE_CAP, OP_EXIT, OP_HOLD_CAP, OP_HOLD_SELF, OP_PROCESS_EXIT, OP_WRITE_SLOT, REPLY_OK,
        encode_request,
    },
    ipc_raw,
};

const STACK_SIZE: u64 = 0x4000;
const EXIT_CODE: u32 = 42;
const PROCESS_EXIT_CODE: u32 = 77;
const REMOVAL_TIMEOUT: Timeout = Timeout::from_secs(5);
const LIFETIME_PROBE: u64 = 0xA11FE;

fn assert_fixture_removed(port: &Port) {
    ipc_raw::write_frame(&encode_request(OP_WRITE_SLOT, LIFETIME_PROBE), &[]);

    // До дренажа таблицы снятого процесса запрос паркуется и получает `PeerClosed` на
    // обнулении множества получателей, после дренажа отказ приходит отзывом деривации.
    kernel_tests::kassert!(matches!(
        port.call(REMOVAL_TIMEOUT).err(),
        Some(Error::Syscall(
            SyscallError::Revoked | SyscallError::PeerClosed
        ))
    ));
}

extern "C" fn exiting_entry(code: usize) -> ! {
    thread_exit(code as u64)
}

#[kernel_test]
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
    kernel_tests::kassert_eq!(thread.exit_code().expect("thread exit code"), EXIT_CODE);
}

/// Потомок, завершившийся `ThreadExit(N)`, наблюдается родителем как
/// терминированный процесс с тем же кодом.
#[kernel_test]
fn child_process_exit_code_is_observable() {
    let fixture = Fixture::spawn();

    let status = fixture.request_status(OP_EXIT, u64::from(EXIT_CODE), &[]);
    kernel_tests::kassert_eq!(status, REPLY_OK);

    fixture
        .process()
        .join(JOIN_TIMEOUT)
        .expect("child terminates");
    kernel_tests::kassert_eq!(
        fixture.process().exit_code().expect("child exit code"),
        EXIT_CODE
    );
}

/// `ProcessExit(N)` снимает процесс вместе с припаркованным потоком, которого
/// `ThreadExit` вызывающего не касается.
#[kernel_test]
fn process_exit_terminates_the_parked_sibling() {
    let fixture = Fixture::spawn();

    let status = fixture.request_status(OP_PROCESS_EXIT, u64::from(PROCESS_EXIT_CODE), &[]);
    kernel_tests::kassert_eq!(status, REPLY_OK);

    fixture
        .process()
        .join(JOIN_TIMEOUT)
        .expect("child terminates");
    kernel_tests::kassert_eq!(
        fixture.process().exit_code().expect("child exit code"),
        PROCESS_EXIT_CODE
    );
}

#[kernel_test]
fn closing_the_last_child_capability_removes_the_child() {
    let fixture = Fixture::spawn();

    // Запрос до закрытия: сессия заведомо жива и обслужена.
    kernel_tests::kassert_eq!(
        fixture.request_status(OP_WRITE_SLOT, LIFETIME_PROBE, &[]),
        REPLY_OK
    );

    let (process, port) = fixture.into_parts();
    drop(process);

    assert_fixture_removed(&port);
}

#[kernel_test]
fn removing_a_holder_removes_the_processes_it_held() {
    let holder = Fixture::spawn();
    let (child_process, child_port) = Fixture::spawn().into_parts();

    // Капа передаётся переносом. Дубликат - производная нашей корневой капы, и её
    // закрытие отзывает дубликат напрямую, минуя каскад.
    let child_cap = child_process.into_raw().raw();
    kernel_tests::kassert_eq!(
        holder.request_status(OP_HOLD_CAP, 0, &[child_cap]),
        REPLY_OK
    );

    let (holder_process, holder_port) = holder.into_parts();
    drop(holder_process);

    assert_fixture_removed(&holder_port);
    assert_fixture_removed(&child_port);
}

#[kernel_test]
fn closing_the_last_holder_from_inside_leaves_the_request_unanswered() {
    let (process, port) = Fixture::spawn().into_parts();

    // Капа передаётся переносом. Единственный держатель остаётся в таблице фикстуры.
    let own_cap = process.into_raw().raw();
    ipc_raw::write_frame(&encode_request(OP_CLOSE_CAP, 0), &[own_cap]);

    kernel_tests::kassert!(matches!(
        port.call(REMOVAL_TIMEOUT).err(),
        Some(Error::Syscall(SyscallError::PeerClosed))
    ));
    assert_fixture_removed(&port);
}

#[kernel_test]
fn a_self_capability_does_not_keep_the_child_alive() {
    let fixture = Fixture::spawn();
    kernel_tests::kassert_eq!(fixture.request_status(OP_HOLD_SELF, 0, &[]), REPLY_OK);

    let (process, port) = fixture.into_parts();
    drop(process);

    assert_fixture_removed(&port);
}
