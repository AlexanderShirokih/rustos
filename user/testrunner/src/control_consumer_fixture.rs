#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use component_client::{ReceivedStartup, component_manifest};
use filesystem_abi::{FileSystemError, PATH_MAX, namespace_admin};
use ipc::wire::Str;
use kernel_objects::{OwnedHandle, Port, Signal};
use runtime::{PortTransport, Timeout, install_log, log_fmt, thread_exit};
use syscall::{Handle, SIGNALED};

const LOG_TAG: &str = "mounts";

const CALL_TIMEOUT: Timeout = Timeout::from_secs(5);

const MOUNT_PREFIX: &str = "/tmp";

const MOUNT_ATTEMPTS: usize = 200;
const MOUNT_TURN: Timeout = Timeout::from_millis(10);

const NO_STARTUP_EXIT_CODE: u64 = 1;
const NO_PARKING_EXIT_CODE: u64 = 2;
const ADMIN_UNREACHABLE_EXIT_CODE: u64 = 3;
const ADMIN_REFUSED_EXIT_CODE: u64 = 4;
const PANIC_EXIT_CODE: u64 = 6;

component_manifest! {
    struct ControlConsumerStartup {
        slots: { mounts: control(protocol = "NamespaceAdmin", instance = "namespace-router") },
    }
}

/// Программа-потребитель control-протокола: снимает монтирование у маршрутизатора имён
/// капабилити своего control-слота и паркуется до снятия. Успех вызова читается по таблице
/// монтирования провайдера, а его отказ уносит ненулевой код выхода. `startup` приходит в x0
/// как HandleId одноразового startup-порта.
#[unsafe(no_mangle)]
pub extern "C" fn _start(startup: Handle) -> ! {
    let Ok(ReceivedStartup { log, resources }) =
        ControlConsumerStartup::receive(OwnedHandle::from_handle(startup))
    else {
        thread_exit(NO_STARTUP_EXIT_CODE);
    };
    install_log(log.into_raw(), LOG_TAG);
    drop(resources.control);
    drop(resources.service);

    let mounts = resources.mounts;
    let failure = unmount(
        &namespace_admin::Client::new(PortTransport::client(&mounts)).with_wait(CALL_TIMEOUT),
    );
    if let Some(code) = failure {
        thread_exit(code);
    }

    // Парковка без кадров: у свежего порта нет отправителей, его приём не вернётся никогда.
    let Ok(parking) = Port::create() else {
        thread_exit(NO_PARKING_EXIT_CODE);
    };
    let _ = parking.recv(Timeout::INFINITE);
    thread_exit(NO_PARKING_EXIT_CODE)
}

fn unmount(admin: &namespace_admin::Client<PortTransport>) -> Option<u64> {
    let Some(prefix) = Str::<PATH_MAX>::new(MOUNT_PREFIX) else {
        return Some(ADMIN_REFUSED_EXIT_CODE);
    };
    for _ in 0..MOUNT_ATTEMPTS {
        match admin.unmount(prefix).map(|response| response.value) {
            Ok(Ok(_)) => return None,
            // Дерево имён загрузки прививает компонент композиции своим оборотом, и до него
            // префикс не занят.
            Ok(Err(FileSystemError::NotFound)) => pause(MOUNT_TURN),
            Ok(Err(error)) => {
                log_fmt(format_args!("unmount refused: {error:?}"));
                return Some(ADMIN_REFUSED_EXIT_CODE);
            }
            Err(error) => {
                log_fmt(format_args!("admin port unreachable: {error:?}"));
                return Some(ADMIN_UNREACHABLE_EXIT_CODE);
            }
        }
    }
    Some(ADMIN_REFUSED_EXIT_CODE)
}

fn pause(timeout: Timeout) {
    let Ok(timer) = Signal::create() else {
        return;
    };
    let _ = timer.wait(SIGNALED, timeout);
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    log_fmt(format_args!("panic: {info}"));
    thread_exit(PANIC_EXIT_CODE)
}
