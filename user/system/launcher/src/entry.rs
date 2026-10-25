use core::{error::Error, fmt, time::Duration};
use std::{
    os::molecular,
    rc::Rc,
    sync::mpsc,
    time::{Instant, SystemTime},
};

use application_abi::{ApplicationError, application};
use bootstrap_abi::system_info;
use component_startup::{AnyOf, Found, Import, Providers, StartupError};
use components::{Attributes, component_manifest, into_std_handle};
use display_abi::display;
use executor::{Executor, PortChannel, Reactor, TaskHandle};
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

use crate::{buttons::Arrow, status, ui};

const NAME_KEY: &str = "name";
const REPLY_TIMEOUT: Timeout = Timeout::from_secs(2);
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
    Closed(u64),
}

struct Home {
    platform: ScreenPlatform,
    executor: Rc<Executor>,
    screen: Import<display::Protocol>,
    apps: Providers<application::Protocol>,
    focus: input_focus::Client<PortChannel<SessionTransport>>,
    keyboard: Keyboard,
    tiles: Vec<&'static App>,
    tiles_input: FocusNumber,
    view: ui::Launcher,
    selected: usize,
    shown: Option<Shown>,
    opened: u64,
    power: Timer,
    commands: mpsc::Sender<Command>,
}

struct Keyboard {
    inject: input_inject::Client<PortChannel<SessionTransport>>,
    device: DeviceNumber,
}

struct Shown {
    name: &'static str,
    opened: u64,
    _application: application::Client<PortChannel<SessionTransport>>,
    _watch: TaskHandle,
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
    async fn run(&mut self, received: &mpsc::Receiver<Command>) -> Result<(), Box<dyn Error>> {
        loop {
            self.view.set_selected(i32::try_from(self.selected)?);
            let command = self.platform.run_until(|| received.try_recv().ok()).await?;
            match command {
                Command::Next if self.shown.is_none() => self.select_next(),
                Command::Previous if self.shown.is_none() => self.select_previous(),
                Command::Open(index) if self.shown.is_none() => self.open(index).await?,
                Command::Home | Command::PowerHeld => self.home().await?,
                Command::Closed(opened)
                    if self
                        .shown
                        .as_ref()
                        .is_some_and(|shown| shown.opened == opened) =>
                {
                    self.closed().await?;
                }
                Command::Reserve(device) => {
                    if let Err(error) = self.focus.reserve(device).await {
                        println!("device {device} not reserved: {error:?}");
                    }
                }
                Command::Arrow(Arrow::Up, pressed) => self.keyboard.set(UP_KEY, pressed).await,
                Command::Arrow(Arrow::Down, pressed) => self.keyboard.set(DOWN_KEY, pressed).await,
                Command::Power(true) => {
                    let commands = self.commands.clone();
                    self.power
                        .start(TimerMode::SingleShot, POWER_HOLD, move || {
                            let _ = send_waking_loop(&commands, Command::PowerHeld);
                        });
                }
                Command::Power(false) if self.power.running() => {
                    self.power.stop();
                    self.keyboard.set(ENTER_KEY, true).await;
                    self.keyboard.set(ENTER_KEY, false).await;
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

    async fn open(&mut self, index: usize) -> Result<(), Box<dyn Error>> {
        let Some(app) = self.tiles.get(index) else {
            return Ok(());
        };

        self.selected = index;
        self.platform.release();
        self.opened = self.opened.wrapping_add(1);
        match open(
            &self.executor,
            &self.screen,
            &self.apps,
            &self.focus,
            app.name,
            self.opened,
            &self.commands,
        )
        .await
        {
            Ok(shown) => {
                self.shown = Some(shown);
            }

            Err(error) => {
                println!("{} not shown: {error}", app.name);
                let display = self.screen.open_async(&self.executor.reactor()).await?;
                self.platform.attach(display).await?;
            }
        }

        Ok(())
    }

    async fn home(&mut self) -> Result<(), Box<dyn Error>> {
        if self.shown.is_none() {
            return Ok(());
        }

        if let Err(error) = self.focus.focus(Some(self.tiles_input)).await {
            println!("tiles not focused: {error:?}");
        }
        // Поля `Shown` закрывают сессию `Application` раньше корня окна, и приложение
        // выходит до отзыва окна.
        self.shown = None;
        let display = self.screen.open_async(&self.executor.reactor()).await?;
        self.platform.attach(display).await?;
        Ok(())
    }

    async fn closed(&mut self) -> Result<(), Box<dyn Error>> {
        if let Some(shown) = &self.shown {
            println!("{} closed", shown.name);
        }

        self.home().await
    }
}

impl Keyboard {
    async fn declare(
        inject: input_inject::Client<PortChannel<SessionTransport>>,
    ) -> Result<Self, Box<dyn Error>> {
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
            .await
            .map_err(|error| format!("virtual keyboard refused: {error:?}"))?;

        Ok(Self { inject, device })
    }

    async fn set(&self, key: u16, pressed: bool) {
        let mut command = input_abi::Command::new();
        let built = if pressed {
            command.press(key)
        } else {
            command.release(key)
        };

        if built.is_ok()
            && let Err(error) = self
                .inject
                .update(self.device, None, command.as_bytes())
                .await
        {
            println!("virtual key not sent: {error:?}");
        }
    }
}

pub(crate) fn main() -> Result<(), Box<dyn Error>> {
    let executor = Rc::new(Executor::new()?);
    executor.block_on(start(&executor))
}

async fn start(executor: &Rc<Executor>) -> Result<(), Box<dyn Error>> {
    let reactor = executor.reactor();
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

    let buttons = PortChannel::client(buttons.open_async(&reactor).await?, reactor.clone())
        .with_wait(Timeout::INFINITE);
    let focus = PortChannel::client(focus.open_async(&reactor).await?, reactor.clone())
        .with_wait(REPLY_TIMEOUT);

    let keyboard = Keyboard::declare(
        PortChannel::client(inject.open_async(&reactor).await?, reactor.clone())
            .with_wait(REPLY_TIMEOUT),
    )
    .await?;

    let FocusSession {
        session: tiles_input,
        number: tiles_number,
    } = focus
        .open()
        .await
        .map_err(|error| format!("tiles input refused: {error:?}"))?;

    focus
        .focus(Some(tiles_number))
        .await
        .map_err(|error| format!("tiles not focused: {error:?}"))?;

    let system = PortChannel::client(system.open_async(&reactor).await?, reactor.clone())
        .with_wait(REPLY_TIMEOUT);

    let clock = install_clock(&rtc, &reactor).await.unwrap_or_else(|error| {
        println!("realtime clock not installed: {error}");
        false
    });

    let display = screen.open_async(&reactor).await?;
    let mut platform = ScreenPlatform::open(display, reactor.clone()).await?;

    let mut tiles = Vec::new();
    for app in &APPS {
        if find(&apps, &reactor, app.name).await.is_ok() {
            tiles.push(app);
        }
    }

    let (commands, received) = mpsc::channel();
    let view = show_tiles(&tiles, &commands)?;
    let _ticker = start_status(executor, &view, system, clock);
    platform.deliver_input(
        executor,
        Session::<input::Protocol>::from_handle(Port::from(tiles_input.into_handle())),
        &view,
    );
    let button_commands = commands.clone();
    let _buttons = executor
        .spawn_now(async move {
            crate::buttons::watch(&buttons, &button_commands).await;
        })
        .abort_on_drop();

    Home {
        platform,
        executor: executor.clone(),
        screen,
        apps,
        focus,
        keyboard,
        tiles,
        tiles_input: tiles_number,
        view,
        selected: 0,
        shown: None,
        opened: 0,
        power: Timer::default(),
        commands,
    }
    .run(&received)
    .await
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
    executor: &Rc<Executor>,
    view: &ui::Launcher,
    system: system_info::Client<PortChannel<SessionTransport>>,
    clock: bool,
) -> TaskHandle {
    let started = Instant::now();
    let weak = view.as_weak();
    let reactor = executor.reactor();
    executor
        .spawn_now(async move {
            loop {
                let snapshot = system.snapshot().await.ok();
                if weak
                    .upgrade_in_event_loop(move |view| {
                        show_status(&view, snapshot, started, clock);
                    })
                    .is_err()
                {
                    return;
                }

                reactor.sleep(Timeout::from_secs(1)).await;
            }
        })
        .abort_on_drop()
}

async fn open(
    executor: &Rc<Executor>,
    screen: &Import<display::Protocol>,
    apps: &Providers<application::Protocol>,
    focus: &input_focus::Client<PortChannel<SessionTransport>>,
    name: &'static str,
    opened: u64,
    commands: &mpsc::Sender<Command>,
) -> Result<Shown, OpenError> {
    let reactor = executor.reactor();
    let found = find(apps, &reactor, name).await?;
    let session = apps
        .open(&reactor, &found)
        .await
        .map_err(OpenError::Startup)?;
    let watched = session
        .handle()
        .duplicate(Rights::WRITE, None)
        .map_err(OpenError::Kernel)?;
    let application: application::Client<PortChannel<SessionTransport>> =
        PortChannel::client(session, reactor.clone()).with_wait(REPLY_TIMEOUT);

    let window = screen
        .open_async(&reactor)
        .await
        .map_err(OpenError::Startup)?;
    let derived = window
        .handle()
        .duplicate(Rights::WRITE | Rights::TRANSFER, None)
        .map_err(OpenError::Kernel)?;

    let FocusSession { session, number } = focus.open().await.map_err(OpenError::Focus)?;
    application
        .show(Session::from_handle(derived), session)
        .await
        .map_err(OpenError::Show)?;
    // Приложение без ввода закрывает сессию `Input`. Фокус тогда снимается, и плитки за окном
    // не получают ввод.
    if focus.focus(Some(number)).await.is_err() {
        focus.focus(None).await.map_err(OpenError::Focus)?;
    }

    let watch = watch_closed(executor, Session::from_handle(watched), opened, commands);
    Ok(Shown {
        name,
        opened,
        _application: application,
        _watch: watch,
        _window: window,
    })
}

fn watch_closed(
    executor: &Rc<Executor>,
    watched: Session<application::Protocol>,
    opened: u64,
    commands: &mpsc::Sender<Command>,
) -> TaskHandle {
    let commands = commands.clone();
    let reactor = executor.reactor();
    executor
        .spawn_now(async move {
            let application = PortChannel::client(watched, reactor).with_wait(Timeout::INFINITE);
            let _ = application.closed().await;
            let _ = send_waking_loop(&commands, Command::Closed(opened));
        })
        .abort_on_drop()
}

async fn install_clock(
    rtc: &Providers<rtc::Protocol>,
    reactor: &Reactor,
) -> Result<bool, Box<dyn Error>> {
    let Some(session) = rtc.open_first(reactor, Attributes::empty()).await? else {
        return Ok(false);
    };

    molecular::install_realtime_clock(into_std_handle(session.into_handle()))?;
    Ok(true)
}

fn show_status(
    view: &ui::Launcher,
    snapshot: Option<bootstrap_abi::SystemSnapshot>,
    started: Instant,
    clock: bool,
) {
    if let Some(snapshot) = snapshot {
        view.set_cpus(status::cpu_text(snapshot).into());
        view.set_memory(status::memory_text(snapshot).into());
    }

    view.set_uptime(status::uptime_text(started.elapsed()).into());
    if let Some(text) = clock.then(SystemTime::now).and_then(status::clock_text) {
        view.set_clock(text.into());
    }
}

async fn find(
    apps: &Providers<application::Protocol>,
    reactor: &Reactor,
    name: &str,
) -> Result<Found, OpenError> {
    apps.all(reactor, AnyOf::new(NAME_KEY, [name]))
        .await
        .map_err(OpenError::Startup)?
        .next(reactor)
        .await
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
