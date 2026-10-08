//! Стенд библиотеки C: проверки контракта `c-library` из C-кода.
//!
//! Rust-часть проверяет путь до установки корня, получает требование `namespace`, ставит
//! файловый корень и передаёт управление C-функции `selftest_main`. Провал проверки пишет её
//! имя в `stderr` и завершает программу кодом 1. После проверок программа регистрирует два
//! обработчика `atexit`, выводит строку без перевода строки и завершается кодом 3: порядок
//! маркеров обработчиков, строка и код показывают путь `exit`.

#![no_std]
#![no_main]

use core::ffi::{CStr, c_char, c_int};

use component_startup::{Port, Session, StartupError, StartupFailure};
use filesystem_abi::namespace;

c_library::c_program!();

c_library::component_manifest! {
    struct Startup {
        requirements: { namespace: session(namespace::Protocol) },
    }
}

#[allow(unsafe_code)]
unsafe extern "C" {
    safe fn selftest_check(passed: c_int, what: *const c_char);
    safe fn selftest_before_root();
    fn selftest_main(argc: c_int, argv: *mut *mut c_char) -> c_int;
}

fn check(passed: bool, what: &CStr) {
    selftest_check(c_int::from(passed), what.as_ptr());
}

fn install_root() {
    let root = c_library::requirements::<Startup>()
        .ok()
        .and_then(|startup| startup.namespace.open().ok());
    let again = c_library::requirements::<Startup>().err();
    check(
        again
            == Some(StartupError {
                requirement: "",
                failure: StartupFailure::NoStartHandle,
            }),
        c"a second request has no start handle",
    );

    let Some(root) = root else {
        check(false, c"the namespace requirement arrives");
        return;
    };
    check(
        c_library::install_filesystem_root(root).is_ok(),
        c"the first root installs",
    );

    let spare = Port::create_service(None).map(|service| Session::from_handle(service.client));
    check(
        spare.is_ok_and(|spare| c_library::install_filesystem_root(spare).is_err()),
        c"a second root comes back",
    );
}

#[allow(unsafe_code)]
#[unsafe(no_mangle)]
extern "C" fn main(count: c_int, arguments: *mut *mut c_char) -> c_int {
    selftest_before_root();
    install_root();
    // SAFETY: `count` и `arguments` - аргументы, которые рантайм передал `main`.
    unsafe { selftest_main(count, arguments) }
}
