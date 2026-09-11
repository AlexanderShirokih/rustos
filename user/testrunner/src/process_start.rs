//! `Process::start` из EL0. На ошибочном пути процесс без загруженного образа отвергает
//! `start`, и переданный по значению хэндл закрывается обёрткой (его `Drop`) ровно один раз,
//! не утекая. Запущенный процесс получает стартовый хэндл без права передачи.

use kernel_objects::{Error, Port, Process, Signal, ThreadEntry, kind};
use kernel_tests::kernel_test;
use runtime::{Priority, Timeout, handle_close, signal_set};
use syscall::{Handle, Rights, SIGNALED, SyscallError, WakeCount};

use crate::fixture::spawn_entry;

const FORWARD_FIXTURE: &str = "/boot/start-handle-forward-fixture/program";
const RECV_TIMEOUT: Timeout = Timeout::from_secs(5);
const JOIN_TIMEOUT: Timeout = Timeout::from_secs(5);

fn is_closed(handle: Handle<kind::Signal>) -> bool {
    signal_set(handle, SIGNALED, 0, WakeCount::None) == SyscallError::BadHandle.as_return_value()
}

#[kernel_test]
fn start_error_closes_passed_handles() {
    let process = Process::create("start-error").expect("process create must succeed");

    let owned = Signal::create().expect("signal create");
    let stale = owned.as_raw();

    let entry = ThreadEntry {
        entry_pc: 0,
        user_sp: 0,
        arg: 0,
        priority: Priority::new(0),
    };

    // Процесс без load_image не стартует: start возвращает ошибку.
    let error = process.start(entry, owned, None).err();
    kernel_tests::kassert!(error.is_some());

    kernel_tests::kassert!(is_closed(stale));
    kernel_tests::kassert_eq!(
        handle_close(stale),
        SyscallError::BadHandle.as_return_value()
    );
}

#[kernel_test]
fn a_started_process_cannot_pass_its_start_handle_on() {
    let port = Port::create().expect("port create");
    let start = port
        .duplicate(Rights::WRITE | Rights::TRANSFER, None)
        .expect("client copy of the port");
    let process = spawn_entry(FORWARD_FIXTURE, start);

    kernel_tests::kassert!(matches!(
        port.recv(RECV_TIMEOUT),
        Err(Error::Syscall(SyscallError::AccessDenied))
    ));

    process.join(JOIN_TIMEOUT).expect("fixture exits");
    kernel_tests::kassert_eq!(
        process.exit_code().expect("exit code"),
        SyscallError::AccessDenied as u32
    );
}
