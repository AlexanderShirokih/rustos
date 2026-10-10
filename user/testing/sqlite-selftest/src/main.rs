//! Стенд sqlite: сценарии спецификации sqlite из C-кода над базой на `/tmp`.
//!
//! Rust-часть получает требование `namespace`, ставит файловый корень и передаёт управление
//! C-функции `selftest_main`. Стенд пишет в `stdout` каждый запрос sqlite и его результат.
//! Провал проверки пишет её имя в `stderr` и даёт код 1, успех всех проверок - код 0.

#![no_std]
#![no_main]

use core::ffi::{c_char, c_int};

use filesystem_abi::namespace;
use sqlite as _;

c_library::c_program!();

c_library::component_manifest! {
    struct Startup {
        requirements: {
            namespace: session(namespace::Protocol),
        },
    }
}

#[allow(unsafe_code)]
unsafe extern "C" {
    safe fn selftest_check(passed: c_int, what: *const c_char);
    fn selftest_main(argc: c_int, argv: *mut *mut c_char) -> c_int;
}

#[allow(unsafe_code)]
#[unsafe(no_mangle)]
extern "C" fn main(count: c_int, arguments: *mut *mut c_char) -> c_int {
    let installed = c_library::requirements::<Startup>()
        .ok()
        .and_then(|startup| startup.namespace.open().ok())
        .is_some_and(|root| c_library::install_filesystem_root(root).is_ok());

    selftest_check(
        c_int::from(installed),
        c"the filesystem root installs".as_ptr(),
    );

    // SAFETY: `count` и `arguments` - аргументы, которые рантайм передал `main`.
    unsafe { selftest_main(count, arguments) }
}
