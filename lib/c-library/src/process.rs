//! Запуск и завершение C-программы поверх рантайма процесса.
//!
//! Запуск повторяет точку входа программы на `std`: неявные импорты компонента, стартовый
//! handle, ключ идентичности потоков, конструкторы библиотеки C и функция `main`. `main`
//! получает в `argv[0]` изменяемую копию имени программы в куче. `exit` библиотеки C выполняет
//! деструкторы `thread_local` вызвавшего потока, затем обработчики `atexit` и деструкторы
//! библиотеки, а затем [`_exit`]. Тот выполняет деструкторы локальных данных потока,
//! выталкивает потоки вывода и завершает процесс.
//!
//! `getentropy` заполняет буфер до 256 байт из источника случайности процесса
//! `runtime::random`.
//!
//! Обработчик паники пишет сообщение в журнал прямой записью `runtime::log_fmt`, мимо потоков
//! вывода, и завершает процесс через `abort`.

#![allow(unsafe_code)]

use core::{
    ffi::{c_char, c_int, c_void},
    num::NonZeroU32,
    panic::PanicInfo,
    ptr, slice,
};

use syscall::Handle;

use crate::{
    ProgramName,
    errno::{Errno, fail},
    memory, stdio, thread,
};

const ENTROPY_MAX_BYTES: usize = 256;

unsafe extern "C" {
    fn __libc_init_array();
    fn main(argc: c_int, argv: *mut *mut c_char) -> c_int;
    fn exit(code: c_int) -> !;
    fn abort() -> !;
}

unsafe extern "Rust" {
    static C_LIBRARY_PROGRAM_NAME: ProgramName;
}

#[unsafe(no_mangle)]
extern "C" fn _start(bootstrap: u32) -> ! {
    runtime::thread::enter_main();

    if let Some(parent) = NonZeroU32::new(bootstrap) {
        runtime::start_component(Handle::from_abi(parent));
    }
    // SAFETY: точка входа исполняется один раз и получает значение `x0` точки входа.
    unsafe { runtime::start(bootstrap) };
    thread::install_identity();
    // SAFETY: конструкторы библиотеки C выполняются один раз до `main`.
    unsafe { __libc_init_array() };

    // SAFETY: статик объявлен макросом `c_program!` и не меняется.
    let program = unsafe { C_LIBRARY_PROGRAM_NAME.as_bytes() };
    let name = memory::duplicate(program);
    if name.is_null() {
        // SAFETY: `abort` библиотеки C не возвращается и завершает процесс ненулевым кодом.
        unsafe { abort() }
    }
    let mut argv = [name.cast::<c_char>(), ptr::null_mut()];
    // SAFETY: `argv` хранит изменяемую строку с завершающим нулём и завершающий `NULL`.
    // Массив остаётся в кадре `_start`, из которого `main` выходит только в `exit`.
    let code = unsafe { main(1, argv.as_mut_ptr()) };
    // SAFETY: `exit` библиотеки C вызывается из потока программы.
    unsafe { exit(code) }
}

#[unsafe(no_mangle)]
extern "C" fn _exit(code: c_int) -> ! {
    runtime::tls::run_destructors();
    stdio::flush();
    runtime::exit(code)
}

#[unsafe(no_mangle)]
extern "C" fn system(command: *const c_char) -> c_int {
    if command.is_null() {
        0
    } else {
        fail(Errno::NoSystem, -1)
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn getentropy(buffer: *mut c_void, length: usize) -> c_int {
    if length > ENTROPY_MAX_BYTES {
        return fail(Errno::InvalidArgument, -1);
    }
    if length == 0 {
        return 0;
    }

    // SAFETY: по контракту POSIX вызывающий передаёт `length` записываемых байт по `buffer`, и
    // никто другой не обращается к ним до возврата; `length` не больше 256.
    let bytes = unsafe { slice::from_raw_parts_mut(buffer.cast::<u8>(), length) };
    runtime::random::fill_bytes(bytes);
    0
}

pub(crate) fn abort_with(message: &str) -> ! {
    runtime::log_line(message);
    // SAFETY: `abort` библиотеки C не возвращается и завершает процесс ненулевым кодом.
    unsafe { abort() }
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    runtime::log_fmt(format_args!("{info}"));
    // SAFETY: `abort` библиотеки C не возвращается и завершает процесс ненулевым кодом.
    unsafe { abort() }
}
