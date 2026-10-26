use alloc::{rc::Rc, sync::Arc};
use core::{
    ffi::{c_char, c_int},
    fmt, ptr,
};

use application_abi::application;
use application_window::{ApplicationWindow, WindowError, accept_window};
use component_startup::{AsyncStartup, StartupError, component_startup as startup_contract};
use display_abi::display;
use executor::{Executor, PortChannel};
use filesystem_abi::namespace;
use input_abi::input;
use kernel_objects::{PortTransport, Session, Signal};
use runtime::{FAILED_EXIT_CODE, Mutex, log_fmt, spawn_owned};
use syscall::{SIGNALED, SyscallError, Timeout};

use crate::{
    keys,
    screen::{self, Screen, ScreenError},
    wad_file::IWAD,
};

c_library::component_manifest! {
    struct Startup {
        requirements: {
            namespace: session(namespace::Protocol),
        },
        exports: [application::Protocol => [{ name = "doom" }]],
    }
}

#[allow(unsafe_code)]
unsafe extern "C" {
    fn doomgeneric_Create(argc: c_int, argv: *mut *mut c_char);
    fn doomgeneric_Tick();
}

#[derive(Debug)]
enum Failure {
    Startup(StartupError),
    RootInstalled,
    Window(WindowError),
    Screen(ScreenError),
    Application(SyscallError),
    Keys(SyscallError),
}

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Startup(error) => error.fmt(formatter),
            Self::RootInstalled => formatter.write_str("filesystem root already installed"),
            Self::Window(error) => error.fmt(formatter),
            Self::Screen(error) => error.fmt(formatter),
            Self::Application(error) => {
                write!(formatter, "application session not served: {error:?}")
            }
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
    let (window, input) = open_window(startup.parent)?;
    let screen = Screen::open(window)?;
    let layout = screen.layout();
    screen::install(screen);
    keys::start(input, layout).map_err(Failure::Keys)?;
    Ok(())
}

fn open_window(
    parent: startup_contract::Client<PortTransport>,
) -> Result<(Session<display::Protocol>, Session<input::Protocol>), Failure> {
    let ready =
        Arc::new(Signal::create().map_err(|error| Failure::Window(WindowError::Kernel(error)))?);
    let shown = Arc::new(Mutex::new(None));
    let output = shown.clone();
    let posted = ready.clone();
    spawn_owned(move || {
        let served: Result<(), WindowError> = Executor::new()
            .map_err(WindowError::Kernel)
            .and_then(|executor| {
                let executor = Rc::new(executor);
                executor.block_on(async {
                    let parent = AsyncStartup::new(PortChannel::from_transport(
                        parent.into_channel(),
                        executor.reactor(),
                    ));
                    let ApplicationWindow {
                        window,
                        input,
                        session,
                    } = accept_window(&executor, parent).await?;
                    *output.lock() = Some(Ok((window, input)));
                    posted.notify_all().map_err(WindowError::Kernel)?;
                    session.serve().await;
                    runtime::exit(0)
                })
            });
        if let Err(error) = served {
            *output.lock() = Some(Err(error));
            let _ = posted.notify_all();
        }
        0
    })
    .map_err(Failure::Application)?;

    ready
        .wait(SIGNALED, Timeout::INFINITE)
        .map_err(|error| Failure::Window(WindowError::Kernel(error)))?;
    shown
        .lock()
        .take()
        .ok_or(Failure::Window(WindowError::Closed))?
        .map_err(Failure::Window)
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
