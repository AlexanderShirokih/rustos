//! Лаунчер показывает плитки приложений и открывает выбранное на весь экран. Шапка показывает
//! время суток, если в системе есть поставщик `Rtc`. Над плитками лаунчер показывает число CPU,
//! занятую память и время со своего запуска.
//!
//! Открытому приложению лаунчер выдаёт производную новой сессии экрана, корень которой держит
//! у себя, а свою сессию экрана на это время закрывает. Вместе с окном приложение получает
//! сессию ввода. Фокус ввода лаунчер переносит на видимое окно, плитки либо приложение.
//! Удержание питания либо клавиша `Home` закрывает сессию `Application` приложения, отзывает
//! окно закрытием корня и возвращает плитки.
//!
//! Кнопки устройства получает только лаунчер. Он переводит громкость вниз и вверх в стрелки вниз и
//! вверх, короткое нажатие питания - в `Enter`. Плитку выбирают стрелками и открывают `Enter`,
//! касанием либо щелчком.

#![feature(molecular_ext)]

use core::{error::Error, fmt, time::Duration};
use std::{
    os::molecular,
    sync::mpsc,
    thread,
    time::{Instant, SystemTime},
};

use application_abi::{ApplicationError, application};
use bootstrap_abi::system_info;
use buttons::Arrow;
use component_startup::{AnyOf, Found, Import, Providers, StartupError};
use components::{Attributes, component_manifest, into_std_handle};
use display_abi::display;
use input_abi::{
    AxisList, Binding, DeviceDescriptor, DeviceNumber, FocusError, FocusNumber, FocusSession,
    KeyList, Usage, input, input_focus, input_inject,
};
use ipc::{CallError, Timeout};
use kernel_objects::{Port, Session, SessionTransport};
use rtc_abi::rtc;
use slint::{ComponentHandle, ModelRc, Timer, TimerMode, VecModel};
use slint_display::ScreenPlatform;
use syscall::Rights;

mod buttons;
mod status;

#[allow(
    unsafe_code,
    unreachable_pub,
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::restriction
)]
mod ui {
    slint::include_modules!();
}

const NAME_KEY: &str = "name";
const REPLY_TIMEOUT: Timeout = Timeout::from_secs(2);
const STATUS_INTERVAL: Duration = Duration::from_secs(1);
const POWER_HOLD: Duration = Duration::from_millis(600);

const VIRTUAL_KEYS: [Usage; 3] = [Usage::UP, Usage::DOWN, Usage::ENTER];
const UP_KEY: u16 = 0;
const DOWN_KEY: u16 = 1;
const ENTER_KEY: u16 = 2;

const APPS: [App; 4] = [
    App {
        name: "doom",
        title: "Doom",
        accent: 0xef_44_44,
    },
    App {
        name: "files",
        title: "Files",
        accent: 0x3b_82_f6,
    },
    App {
        name: "journal",
        title: "Journal",
        accent: 0x22_c5_5e,
    },
    App {
        name: "physics",
        title: "Physics",
        accent: 0xf5_9e_0b,
    },
];

component_manifest! {
    struct LauncherStartup {
        requirements: {
            screen: session(display::Protocol),
            apps: all(application::Protocol),
            buttons: session(input::Protocol),
            focus: session(input_focus::Protocol),
            inject: session(input_inject::Protocol),
            system: session(system_info::Protocol),
            rtc: all(rtc::Protocol),
        },
    }
}

struct App {
    name: &'static str,
    title: &'static str,
    accent: u32,
}

#[derive(Debug)]
pub(crate) enum Command {
    Next,
    Previous,
    Open(usize),
    Home,
    Reserve(DeviceNumber),
    Arrow(Arrow, bool),
    Power(bool),
    PowerHeld,
    PowerLost,
}

struct Home {
    platform: ScreenPlatform,
    screen: Import<display::Protocol>,
    apps: Providers<application::Protocol>,
    focus: input_focus::Client<SessionTransport>,
    keyboard: Keyboard,
    tiles: Vec<&'static App>,
    tiles_input: FocusNumber,
    view: ui::Launcher,
    selected: usize,
    shown: Option<Shown>,
    power: Timer,
    commands: mpsc::Sender<Command>,
}

struct Keyboard {
    inject: input_inject::Client<SessionTransport>,
    device: DeviceNumber,
}

struct Shown {
    _application: application::Client<SessionTransport>,
    _window: Session<display::Protocol>,
}

#[derive(Debug)]
enum OpenError {
    Startup(StartupError),
    NotFound,
    Kernel(kernel_objects::Error),
    Focus(CallError<FocusError>),
    Show(CallError<ApplicationError>),
}

impl fmt::Display for OpenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Startup(error) => error.fmt(formatter),
            Self::NotFound => formatter.write_str("application not found"),
            Self::Kernel(error) => error.fmt(formatter),
            Self::Focus(error) => write!(formatter, "input focus refused: {error:?}"),
            Self::Show(error) => write!(formatter, "show refused: {error:?}"),
        }
    }
}

impl Error for OpenError {}

impl Home {
    fn run(&mut self, received: &mpsc::Receiver<Command>) -> Result<(), Box<dyn Error>> {
        loop {
            self.view.set_selected(i32::try_from(self.selected)?);
            let command = self.platform.run_until(|| received.try_recv().ok())?;
            match command {
                Command::Next if self.shown.is_none() => self.select_next(),
                Command::Previous if self.shown.is_none() => self.select_previous(),
                Command::Open(index) if self.shown.is_none() => self.open(index)?,
                Command::Home | Command::PowerHeld => self.home()?,
                Command::Reserve(device) => {
                    if let Err(error) = self.focus.reserve(device) {
                        println!("device {device} not reserved: {error:?}");
                    }
                }
                Command::Arrow(Arrow::Up, pressed) => self.keyboard.set(UP_KEY, pressed),
                Command::Arrow(Arrow::Down, pressed) => self.keyboard.set(DOWN_KEY, pressed),
                Command::Power(true) => {
                    let commands = self.commands.clone();
                    self.power
                        .start(TimerMode::SingleShot, POWER_HOLD, move || {
                            let _ = send_waking_loop(&commands, Command::PowerHeld);
                        });
                }
                Command::Power(false) if self.power.running() => {
                    self.power.stop();
                    self.keyboard.set(ENTER_KEY, true);
                    self.keyboard.set(ENTER_KEY, false);
                }
                Command::PowerLost => self.power.stop(),
                _ => {}
            }
        }
    }

    fn select_next(&mut self) {
        self.selected = self
            .selected
            .saturating_add(1)
            .checked_rem(self.tiles.len())
            .unwrap_or(0);
    }

    fn select_previous(&mut self) {
        self.selected = self
            .selected
            .checked_sub(1)
            .unwrap_or_else(|| self.tiles.len().saturating_sub(1));
    }

    fn open(&mut self, index: usize) -> Result<(), Box<dyn Error>> {
        let Some(app) = self.tiles.get(index) else {
            return Ok(());
        };
        self.selected = index;
        self.platform.release();
        match open(&self.screen, &self.apps, &self.focus, app.name) {
            Ok(shown) => self.shown = Some(shown),
            Err(error) => {
                println!("{} not shown: {error}", app.name);
                self.platform.attach(self.screen.open()?.client())?;
            }
        }
        Ok(())
    }

    fn home(&mut self) -> Result<(), Box<dyn Error>> {
        if self.shown.is_none() {
            return Ok(());
        }

        if let Err(error) = self.focus.focus(Some(self.tiles_input)) {
            println!("tiles not focused: {error:?}");
        }
        // Поля `Shown` закрывают сессию `Application` раньше корня окна, и приложение
        // выходит до отзыва окна.
        self.shown = None;
        self.platform.attach(self.screen.open()?.client())?;
        Ok(())
    }
}

impl Keyboard {
    fn declare(inject: input_inject::Client<SessionTransport>) -> Result<Self, Box<dyn Error>> {
        let mut keys = KeyList::new();
        for usage in VIRTUAL_KEYS {
            keys.push(usage)
                .map_err(|error| format!("key list: {error:?}"))?;
        }
        let no_axes = AxisList::new();
        let descriptor =
            DeviceDescriptor::new("launcher keys", Binding::None, &keys, &no_axes, 0, &no_axes)
                .ok_or("virtual keyboard name too long")?;
        let device = inject
            .declare(descriptor)
            .map_err(|error| format!("virtual keyboard refused: {error:?}"))?;
        Ok(Self { inject, device })
    }

    fn set(&self, key: u16, pressed: bool) {
        let mut command = input_abi::Command::new();
        let built = if pressed {
            command.press(key)
        } else {
            command.release(key)
        };
        if built.is_ok()
            && let Err(error) = self.inject.update(self.device, None, command.as_bytes())
        {
            println!("virtual key not sent: {error:?}");
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let LauncherStartup {
        screen,
        apps,
        buttons,
        focus,
        inject,
        system,
        rtc,
        ..
    } = components::requirements::<LauncherStartup>()?;
    let buttons = buttons.open()?.client().with_wait(Timeout::INFINITE);
    let focus = focus.open()?.client().with_wait(REPLY_TIMEOUT);
    let keyboard = Keyboard::declare(inject.open()?.client().with_wait(REPLY_TIMEOUT))?;
    let FocusSession {
        session: tiles_input,
        number: tiles_number,
    } = focus
        .open()
        .map_err(|error| format!("tiles input refused: {error:?}"))?;
    focus
        .focus(Some(tiles_number))
        .map_err(|error| format!("tiles not focused: {error:?}"))?;
    let system = system.open()?.client().with_wait(REPLY_TIMEOUT);
    let clock = install_clock(&rtc).unwrap_or_else(|error| {
        println!("realtime clock not installed: {error}");
        false
    });
    let platform = ScreenPlatform::open(screen.open()?.client())?;

    let tiles = APPS
        .iter()
        .filter(|app| find(&apps, app.name).is_ok())
        .collect::<Vec<_>>();
    let (commands, received) = mpsc::channel();
    let view = show_tiles(&tiles, &commands)?;
    let _ticker = start_status(&view, system, clock);
    platform.deliver_input(
        Session::<input::Protocol>::from_handle(Port::from(tiles_input.into_handle()))
            .client()
            .with_wait(Timeout::INFINITE),
        &view,
    );
    let button_commands = commands.clone();
    thread::spawn(move || buttons::watch(&buttons, &button_commands));

    Home {
        platform,
        screen,
        apps,
        focus,
        keyboard,
        tiles,
        tiles_input: tiles_number,
        view,
        selected: 0,
        shown: None,
        power: Timer::default(),
        commands,
    }
    .run(&received)
}

fn show_tiles(
    tiles: &[&App],
    commands: &mpsc::Sender<Command>,
) -> Result<ui::Launcher, Box<dyn Error>> {
    let view = ui::Launcher::new().map_err(|error| format!("ui refused: {error:?}"))?;
    let sender = commands.clone();
    view.on_next(move || drop(sender.send(Command::Next)));
    let sender = commands.clone();
    view.on_previous(move || drop(sender.send(Command::Previous)));
    let sender = commands.clone();
    view.on_open(move |index| {
        if let Ok(index) = usize::try_from(index) {
            let _ = sender.send(Command::Open(index));
        }
    });
    view.set_tiles(ModelRc::new(VecModel::from(
        tiles
            .iter()
            .map(|app| ui::Tile {
                title: app.title.into(),
                letter: app.title.get(..1).unwrap_or_default().into(),
                accent: slint::Color::from_argb_encoded(0xff00_0000 | app.accent),
            })
            .collect::<Vec<_>>(),
    )));
    view.show()
        .map_err(|error| format!("window refused: {error:?}"))?;
    Ok(view)
}

fn start_status(
    view: &ui::Launcher,
    system: system_info::Client<SessionTransport>,
    clock: bool,
) -> Timer {
    let started = Instant::now();
    show_status(view, &system, started, clock);
    let ticker = Timer::default();
    let weak = view.as_weak();
    ticker.start(TimerMode::Repeated, STATUS_INTERVAL, move || {
        if let Some(view) = weak.upgrade() {
            show_status(&view, &system, started, clock);
        }
    });
    ticker
}

fn open(
    screen: &Import<display::Protocol>,
    apps: &Providers<application::Protocol>,
    focus: &input_focus::Client<SessionTransport>,
    name: &str,
) -> Result<Shown, OpenError> {
    let found = find(apps, name)?;
    let application: application::Client<SessionTransport> = apps
        .open(&found)
        .map_err(OpenError::Startup)?
        .client()
        .with_wait(REPLY_TIMEOUT);

    let window = screen.open().map_err(OpenError::Startup)?;
    let derived = window
        .handle()
        .duplicate(Rights::WRITE | Rights::TRANSFER, None)
        .map_err(OpenError::Kernel)?;
    let FocusSession { session, number } = focus.open().map_err(OpenError::Focus)?;
    application
        .show(Session::from_handle(derived), session)
        .map_err(OpenError::Show)?;
    // Приложение без ввода закрывает сессию `Input`. Фокус тогда снимается, и плитки за окном
    // не получают ввод.
    if focus.focus(Some(number)).is_err() {
        focus.focus(None).map_err(OpenError::Focus)?;
    }

    Ok(Shown {
        _application: application,
        _window: window,
    })
}

fn install_clock(rtc: &Providers<rtc::Protocol>) -> Result<bool, Box<dyn Error>> {
    let Some(found) = rtc.all(Attributes::empty())?.next().transpose()? else {
        return Ok(false);
    };
    let session = rtc.open(&found)?;
    molecular::install_realtime_clock(into_std_handle(session.into_handle()))?;
    Ok(true)
}

fn show_status(
    view: &ui::Launcher,
    system: &system_info::Client<SessionTransport>,
    started: Instant,
    clock: bool,
) {
    if let Ok(snapshot) = system.snapshot() {
        view.set_cpus(status::cpu_text(snapshot).into());
        view.set_memory(status::memory_text(snapshot).into());
    }
    view.set_uptime(status::uptime_text(started.elapsed()).into());
    if let Some(text) = clock.then(SystemTime::now).and_then(status::clock_text) {
        view.set_clock(text.into());
    }
}

fn find(apps: &Providers<application::Protocol>, name: &str) -> Result<Found, OpenError> {
    apps.all(AnyOf::new(NAME_KEY, [name]))
        .map_err(OpenError::Startup)?
        .next()
        .ok_or(OpenError::NotFound)?
        .map_err(OpenError::Startup)
}

pub(crate) fn send_waking_loop(
    commands: &mpsc::Sender<Command>,
    command: Command,
) -> Result<(), slint::EventLoopError> {
    let commands = commands.clone();
    slint::invoke_from_event_loop(move || drop(commands.send(command)))
}
