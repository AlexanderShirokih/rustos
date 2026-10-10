//! Порт движка doomgeneric на экран протокола `Display` и клавиши протокола `Input`.
//!
//! Программа ставит файловый корень из требования `namespace`, открывает экран требования
//! `screen`, запускает чтение клавиш требования `input` и исполняет тики движка. IWAD движка
//! должен находиться в файловом корне по пути `/boot/doom/freedoom1.wad`. Отказ требования,
//! экрана или запуска чтения клавиш программа пишет в журнал и завершается кодом
//! `FAILED_EXIT_CODE`. Движок завершает программу через `exit` библиотеки C.

#![no_std]
#![no_main]

extern crate alloc;

mod frame;
mod keys;
mod platform;
mod screen;

use core::{
    ffi::{CStr, c_char, c_int},
    fmt, ptr,
};

use component_startup::StartupError;
use display_abi::display;
use filesystem_abi::namespace;
use input_abi::input;
use runtime::{FAILED_EXIT_CODE, log_fmt};
use syscall::SyscallError;

use crate::screen::{Screen, ScreenError};

c_library::c_program!();

c_library::component_manifest! {
    struct Startup {
        requirements: {
            namespace: session(namespace::Protocol),
            screen: session(display::Protocol),
            input: session(input::Protocol),
        },
    }
}

const IWAD: &CStr = c"/boot/doom/freedoom1.wad";

#[allow(unsafe_code)]
unsafe extern "C" {
    fn doomgeneric_Create(argc: c_int, argv: *mut *mut c_char);
    fn doomgeneric_Tick();
}

#[derive(Debug)]
enum Failure {
    Startup(StartupError),
    RootInstalled,
    Screen(ScreenError),
    Keys(SyscallError),
}

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Startup(error) => error.fmt(formatter),
            Self::RootInstalled => formatter.write_str("filesystem root already installed"),
            Self::Screen(error) => error.fmt(formatter),
            Self::Keys(error) => write!(formatter, "input reader not started: {error:?}"),
        }
    }
}

impl From<StartupError> for Failure {
    fn from(error: StartupError) -> Self {
        Self::Startup(error)
    }
}

impl From<ScreenError> for Failure {
    fn from(error: ScreenError) -> Self {
        Self::Screen(error)
    }
}

fn start() -> Result<(), Failure> {
    let startup = c_library::requirements::<Startup>()?;
    c_library::install_filesystem_root(startup.namespace.open()?)
        .map_err(|_| Failure::RootInstalled)?;
    screen::install(Screen::open(startup.screen.open()?)?);
    keys::start(startup.input.open()?).map_err(Failure::Keys)?;
    Ok(())
}

#[allow(unsafe_code, clippy::multiple_unsafe_ops_per_block)]
#[unsafe(no_mangle)]
extern "C" fn main(_count: c_int, _arguments: *mut *mut c_char) -> c_int {
    if let Err(failure) = start() {
        log_fmt(format_args!("{failure}"));
        return FAILED_EXIT_CODE;
    }

    let mut arguments = [
        c"doomgeneric".as_ptr().cast_mut(),
        c"-iwad".as_ptr().cast_mut(),
        IWAD.as_ptr().cast_mut(),
        ptr::null_mut(),
    ];
    // SAFETY: строки аргументов завершены нулём, массив завершён `NULL`. Движок сохраняет
    // массив и читает его строки без записи. Массив остаётся в кадре `main`, из которого
    // управление уходит только в `exit` движка. Тики идут после `doomgeneric_Create` из того
    // же потока.
    unsafe {
        doomgeneric_Create(3, arguments.as_mut_ptr());
        loop {
            doomgeneric_Tick();
        }
    }
}
