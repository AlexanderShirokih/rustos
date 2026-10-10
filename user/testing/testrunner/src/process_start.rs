//! `Process::start` из EL0. На ошибочном пути процесс без загруженного образа отвергает
//! `start`, и переданный по значению хэндл закрывает его владелец ровно один раз,
//! не утекая. Запущенный процесс получает стартовый хэндл без права передачи.

use kernel_objects::{Error, Port, Process, Signal, ThreadEntry, kind};
use runtime::{Priority, Timeout, signal_set};
use syscall::{Handle, ProcessName, Rights, SIGNALED, SyscallError, WakeCount};

use crate::fixture::spawn_entry;

const FORWARD_FIXTURE: &str = "/start-handle-forward-fixture/exec";
const RECV_TIMEOUT: Timeout = Timeout::from_millis(100);
const RECV_ATTEMPTS: usize = 50;

fn is_closed(handle: Handle<kind::Signal>) -> bool {
    signal_set(handle, SIGNALED, 0, WakeCount::None) == SyscallError::BadHandle.as_return_value()
}

#[test_harness::test]
fn start_error_closes_passed_handles() {
    let process = Process::create(ProcessName::new("start-error").expect("process name"))
        .expect("process create must succeed");

    let owned = Signal::create().expect("signal create");
    let stale = owned.as_raw();

    let entry = ThreadEntry {
        entry_pc: 0,
        user_sp: 0,
        arg: 0,
        priority: Priority::new(0),
    };

    let error = process.start(entry, owned, None).err();
    test_harness::check!(error.is_some());

    test_harness::check!(is_closed(stale));
}

#[test_harness::test]
fn a_started_process_cannot_pass_its_start_handle_on() {
    let port = Port::create().expect("port create");
    let start = port
        .duplicate(Rights::WRITE | Rights::TRANSFER, None)
        .expect("client copy of the port");
    let process = spawn_entry(FORWARD_FIXTURE, start);

    // Передачу хэндла порт отклоняет при встрече с получателем, поэтому тест принимает,
    // пока фикстура не завершится.
    for _ in 0..RECV_ATTEMPTS {
        test_harness::check!(matches!(
            port.recv(RECV_TIMEOUT),
            Err(Error::Syscall(SyscallError::Timeout))
        ));

        if process.join(Timeout::POLL).is_ok() {
            break;
        }
    }

    process.join(Timeout::POLL).expect("fixture exits");
    test_harness::check_eq!(
        process.exit_code().expect("exit code"),
        SyscallError::AccessDenied as u32
    );
}
