//! Стенд библиотеки C: проверки контракта `c-library` из C-кода.
//!
//! Rust-часть проверяет путь до установки корня, получает требование `namespace` и ставит
//! файловый корень, проверяет часы до установки часов реального времени, ставит их из
//! требования `rtc` и передаёт управление C-функции `selftest_main`. Провал проверки пишет её
//! имя в `stderr` и завершает программу кодом 1. После проверок программа регистрирует два
//! обработчика `atexit`, выводит строку без перевода строки и завершается кодом 3: порядок
//! маркеров обработчиков, строка и код показывают путь `exit`.

#![no_std]
#![no_main]

use core::ffi::{CStr, c_char, c_int};

use c_library::RealtimeClockError;
use component_startup::{Port, Session, StartupError, StartupFailure};
use filesystem_abi::namespace;
use rtc_abi::rtc;

c_library::c_program!();

c_library::component_manifest! {
    struct Startup {
        requirements: {
            namespace: session(namespace::Protocol),
            rtc: session(rtc::Protocol),
        },
    }
}

#[allow(unsafe_code)]
unsafe extern "C" {
    safe fn selftest_check(passed: c_int, what: *const c_char);
    safe fn selftest_before_root();
    safe fn selftest_before_clock();
    fn selftest_main(argc: c_int, argv: *mut *mut c_char) -> c_int;
}

fn check(passed: bool, what: &CStr) {
    selftest_check(c_int::from(passed), what.as_ptr());
}

fn startup() -> Option<Startup> {
    let startup = c_library::requirements::<Startup>().ok();
    let again = c_library::requirements::<Startup>().err();
    check(
        again
            == Some(StartupError {
                requirement: "",
                failure: StartupFailure::NoStartHandle,
            }),
        c"a second request has no start handle",
    );
    startup
}

fn client_without_server() -> Option<Port> {
    Port::create_service(None)
        .ok()
        .map(|service| service.client)
}

fn install_root(startup: Option<&Startup>) {
    let Some(root) = startup.and_then(|startup| startup.namespace.open().ok()) else {
        check(false, c"the namespace requirement arrives");
        return;
    };
    check(
        c_library::install_filesystem_root(root).is_ok(),
        c"the first root installs",
    );

    check(
        client_without_server().is_some_and(|client| {
            c_library::install_filesystem_root(Session::from_handle(client)).is_err()
        }),
        c"a second root comes back",
    );
}

fn install_clock(startup: Option<&Startup>) {
    let Some(clock) = startup.and_then(|startup| startup.rtc.open().ok()) else {
        check(false, c"the rtc requirement arrives");
        return;
    };
    check(
        c_library::install_realtime_clock(clock).is_ok(),
        c"the first realtime clock installs",
    );

    check(
        client_without_server().is_some_and(|client| {
            matches!(
                c_library::install_realtime_clock(Session::from_handle(client)),
                Err(RealtimeClockError::AlreadyInstalled)
            )
        }),
        c"a second realtime clock is refused",
    );
}

#[allow(unsafe_code)]
#[unsafe(no_mangle)]
extern "C" fn main(count: c_int, arguments: *mut *mut c_char) -> c_int {
    selftest_before_root();
    let startup = startup();
    install_root(startup.as_ref());
    selftest_before_clock();
    install_clock(startup.as_ref());
    // SAFETY: `count` и `arguments` - аргументы, которые рантайм передал `main`.
    unsafe { selftest_main(count, arguments) }
}
