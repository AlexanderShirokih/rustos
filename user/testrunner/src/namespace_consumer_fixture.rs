#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use component_startup::{ReceivedStartup, component_manifest};
use filesystem_abi::{PATH_MAX, namespace};
use ipc::wire::Str;
use kernel_objects::{OwnedHandle, Signal};
use runtime::{PortTransport, Timeout, thread_exit};
use syscall::{Handle, SIGNALED};

const PROBE_PATH: &str = "/tmp";

const CALL_TIMEOUT: Timeout = Timeout::from_secs(2);

const PROBE_TURN: Timeout = Timeout::from_millis(10);

const NO_STARTUP_EXIT_CODE: u64 = 1;
const NO_ANSWER_EXIT_CODE: u64 = 3;
const PROVIDER_GONE_EXIT_CODE: u64 = 9;

component_manifest! {
    struct ConsumerStartup {
        slots: { namespace: service(protocol = "Namespace") },
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(startup: Handle) -> ! {
    let Ok(ReceivedStartup { log, resources }) =
        ConsumerStartup::receive(OwnedHandle::from_handle(startup))
    else {
        thread_exit(NO_STARTUP_EXIT_CODE);
    };
    drop(log);

    let session = resources.namespace;
    let namespace = namespace::Client::new(PortTransport::client(&session)).with_wait(CALL_TIMEOUT);
    let Some(path) = Str::<{ PATH_MAX }>::new(PROBE_PATH) else {
        thread_exit(NO_ANSWER_EXIT_CODE);
    };
    if !answers(&namespace, path) {
        thread_exit(NO_ANSWER_EXIT_CODE);
    }

    // Провайдер отвечает по сессии слота, пока жив. Его уход виден отказом транспорта, а путь
    // вне таблицы монтирования - обычный ответ.
    while answers(&namespace, path) {
        pause(PROBE_TURN);
    }
    thread_exit(PROVIDER_GONE_EXIT_CODE)
}

fn answers(namespace: &namespace::Client<PortTransport>, path: Str<'_, { PATH_MAX }>) -> bool {
    // Доменный отказ - тоже ответ; выданную сессию закрывает сам ответ.
    namespace.resolve(path).is_ok()
}

fn pause(timeout: Timeout) {
    let Ok(timer) = Signal::create() else {
        return;
    };
    let _ = timer.wait(SIGNALED, timeout);
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
