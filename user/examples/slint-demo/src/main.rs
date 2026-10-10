//! Рабочий стол на Slint с двумя окнами. Файловый менеджер показывает смонтированные файловые
//! системы, содержимое выбранного каталога и начало выбранного файла. Окно физики показывает
//! упругие столкновения шаров. Строка состояния показывает частоту кадров и время RTC, а без
//! поставщика RTC - время с запуска. Раскладка следует ориентации экрана.
//! Указатель и клавиши сессии `Input` управляют интерфейсом, и нажатие выбирает файловую
//! систему, каталог и файл.
//! Программный рендерер Slint рисует в поверхность сессии `Display` и переносит изменённую
//! область кадра на экран.

#![feature(molecular_ext)]

use core::{error::Error, time::Duration};
use std::{
    collections::VecDeque,
    fs,
    io::{self, Read},
    os::molecular,
    rc::Rc,
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, RecvTimeoutError},
    },
    thread,
    time::Instant,
};

use components::{Attributes, Providers, component_manifest, into_std_handle};
use display_abi::{Area, DisplayDimension, display};
use filesystem_abi::namespace;
use input_abi::input;
use kernel_objects::{MemoryRegion, SessionTransport};
use rtc_abi::rtc;
use slint::{
    ComponentHandle, EventLoopError, Model, ModelRc, PhysicalSize, PlatformError, VecModel,
    platform::{
        EventLoopProxy, Platform, WindowAdapter, WindowEvent,
        software_renderer::{
            MinimalSoftwareWindow, PhysicalRegion, PremultipliedRgbaColor, RepaintBufferType,
            TargetPixel,
        },
    },
};
use syscall::{Timeout, UserMemFlags};

mod events;
mod physics;

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

const CALL_TIMEOUT: Timeout = Timeout::from_secs(2);

const FRAME_INTERVAL: Duration = Duration::from_millis(16);
const RESCAN_INTERVAL: Duration = Duration::from_secs(5);

const LANDSCAPE_WIDTH: f32 = 1280.0;
const PORTRAIT_WIDTH: f32 = 540.0;
const LANDSCAPE_FIELD_WIDTH: f32 = 360.0;
const PORTRAIT_FIELD_HEIGHT: f32 = 280.0;
const FIELD_CHROME_HEIGHT: f32 = 134.0;
const FIELD_CHROME_WIDTH: f32 = 34.0;

const BODY_COUNT: u16 = 48;
const STEP_INTERVAL: Duration = Duration::from_millis(8);
const TINTS: [u32; 4] = [0xd9_25_63_eb, 0xd9_0e_a5_e9, 0xd9_f5_9e_0b, 0xd9_e1_1d_48];

const MOUNTS: [(&str, &str); 2] = [("/", "image, read-only"), ("/tmp", "ramfs")];
const FOLDER: &str = "slint-demo";
const FILE: &str = "system.toml";
const FILE_LINES: usize = 34;
const PREVIEW_BYTES_MAX: u64 = 4096;

const SECONDS_PER_HOUR: u64 = 3_600;
const SECONDS_PER_DAY: u64 = 86_400;
const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

component_manifest! {
    struct SlintDemoStartup {
        requirements: {
            screen: session(display::Protocol),
            namespace: session(namespace::Protocol),
            input: session(input::Protocol),
            rtc: all(rtc::Protocol),
        },
    }
}

#[repr(transparent)]
#[derive(Clone, Copy)]
struct Xrgb(u32);

type Task = Box<dyn FnOnce() + Send>;

struct Layout {
    portrait: bool,
    scale: f32,
    field_width: f32,
    field_height: f32,
}

struct Surface<'a> {
    screen: display::Client<SessionTransport>,
    pixels: &'a mut [Xrgb],
    stride: usize,
}

#[derive(Default)]
struct Simulation {
    centers: Vec<(f32, f32)>,
    steps: u32,
    busy: Duration,
}

#[derive(Default)]
struct Second {
    frames: u32,
    drawing: Duration,
    steps: u32,
    busy: Duration,
}

struct ScreenPlatform {
    window: Rc<MinimalSoftwareWindow>,
    start: Instant,
    tasks: Arc<Mutex<VecDeque<Task>>>,
}

struct TaskProxy {
    tasks: Arc<Mutex<VecDeque<Task>>>,
}

#[derive(Clone)]
struct Selection {
    mount: String,
    folder: String,
    file: String,
}

struct Scan {
    root: Vec<ui::Entry>,
    folder: Vec<ui::Entry>,
    text: String,
}

impl TargetPixel for Xrgb {
    fn blend(&mut self, color: PremultipliedRgbaColor) {
        let keep = u32::from(u8::MAX - color.alpha);
        let channel = |shift: u32, add: u8| {
            let blended = ((self.0 >> shift) & 0xff) * keep / 255 + u32::from(add);
            blended.min(0xff) << shift
        };
        self.0 = channel(16, color.red) | channel(8, color.green) | channel(0, color.blue);
    }

    fn from_rgb(red: u8, green: u8, blue: u8) -> Self {
        Self(u32::from(red) << 16 | u32::from(green) << 8 | u32::from(blue))
    }
}

impl Platform for ScreenPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.window.clone())
    }

    fn duration_since_start(&self) -> Duration {
        self.start.elapsed()
    }

    fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> {
        Some(Box::new(TaskProxy {
            tasks: self.tasks.clone(),
        }))
    }
}

impl EventLoopProxy for TaskProxy {
    fn quit_event_loop(&self) -> Result<(), EventLoopError> {
        Err(EventLoopError::NoEventLoopProvider)
    }

    fn invoke_from_event_loop(&self, event: Task) -> Result<(), EventLoopError> {
        self.tasks
            .lock()
            .map_err(|_| EventLoopError::EventLoopTerminated)?
            .push_back(event);
        Ok(())
    }
}

impl Layout {
    fn new(dimension: DisplayDimension) -> Self {
        // Масштаб растягивает раскладку на экран шире расчётного, логическая ширина не опускается
        // ниже расчётной ширины ориентации.
        let width = f32::from(u16::try_from(dimension.width).unwrap_or(u16::MAX));
        let height = f32::from(u16::try_from(dimension.height).unwrap_or(u16::MAX));
        let portrait = height > width;
        let design_width = if portrait {
            PORTRAIT_WIDTH
        } else {
            LANDSCAPE_WIDTH
        };
        let scale = (width / design_width).max(1.0);
        let (field_width, field_height) = if portrait {
            (width / scale - FIELD_CHROME_WIDTH, PORTRAIT_FIELD_HEIGHT)
        } else {
            (LANDSCAPE_FIELD_WIDTH, height / scale - FIELD_CHROME_HEIGHT)
        };
        Self {
            portrait,
            scale,
            field_width,
            field_height,
        }
    }
}

impl Scan {
    fn collect(selection: &Selection) -> io::Result<Self> {
        let folder = child_path(&selection.mount, &selection.folder);
        Ok(Self {
            root: entries(&selection.mount)?,
            folder: if selection.folder.is_empty() {
                Vec::new()
            } else {
                entries(&folder)?
            },
            text: if selection.file.is_empty() {
                String::new()
            } else {
                preview(&child_path(&folder, &selection.file))?
            },
        })
    }

    fn show(self, demo: &ui::Demo) {
        let lines: Vec<ui::Line> = self
            .text
            .lines()
            .take(FILE_LINES)
            .zip(1..)
            .map(|(line, number)| text_line(line, number))
            .collect();

        demo.global::<ui::Desktop<'_>>()
            .set_root_entries(ModelRc::new(VecModel::from(self.root)));
        demo.global::<ui::Desktop<'_>>()
            .set_folder_entries(ModelRc::new(VecModel::from(self.folder)));
        demo.global::<ui::Desktop<'_>>()
            .set_file_lines(ModelRc::new(VecModel::from(lines)));
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let requirements = components::requirements::<SlintDemoStartup>()?;
    let screen = requirements.screen.open()?.client().with_wait(CALL_TIMEOUT);
    let namespace = requirements.namespace.open()?;
    let input = requirements.input.open()?.client();
    molecular::install_filesystem_root(into_std_handle(namespace.into_handle()))
        .map_err(|_| "filesystem root is already installed")?;
    let epoch_seconds = rtc_now(&requirements.rtc)?;
    if epoch_seconds.is_none() {
        println!("RTC provider is unavailable, showing uptime");
    }

    let dimension = screen.dimension()?;
    let region = MemoryRegion::from(
        screen
            .acquire_surface()
            .map_err(|error| format!("surface refused: {error:?}"))?,
    );
    let mut mapping = kernel_objects::Mapping::full(region, UserMemFlags::ReadWrite)?;
    let pixels = surface_pixels(&mut mapping, dimension)?;

    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    let tasks = Arc::new(Mutex::new(VecDeque::new()));
    slint::platform::set_platform(Box::new(ScreenPlatform {
        window: window.clone(),
        start: Instant::now(),
        tasks: tasks.clone(),
    }))
    .map_err(|error| format!("platform refused: {error:?}"))?;
    let layout = Layout::new(dimension);
    window.set_size(PhysicalSize::new(dimension.width, dimension.height));
    window.dispatch_event(WindowEvent::ScaleFactorChanged {
        scale_factor: layout.scale,
    });

    let demo = ui::Demo::new().map_err(|error| format!("ui refused: {error:?}"))?;
    demo.set_portrait(layout.portrait);
    let desktop = demo.global::<ui::Desktop<'_>>();
    desktop.set_places(ModelRc::new(VecModel::from(
        MOUNTS
            .iter()
            .map(|(path, note)| ui::Place {
                path: (*path).into(),
                note: (*note).into(),
            })
            .collect::<Vec<_>>(),
    )));
    start_file_scan(&demo);
    demo.show()
        .map_err(|error| format!("window refused: {error:?}"))?;
    println!(
        "desktop on {}x{}, scale {}",
        dimension.width, dimension.height, layout.scale
    );

    let weak = demo.as_weak();
    let bounds = events::Screen {
        width: f32::from(u16::try_from(dimension.width).unwrap_or(u16::MAX)),
        height: f32::from(u16::try_from(dimension.height).unwrap_or(u16::MAX)),
        scale: layout.scale,
    };
    thread::spawn(move || read_input(&input, bounds, &weak));

    let world = physics::World::new(
        BODY_COUNT,
        layout.field_width,
        layout.field_height,
        epoch_seconds.unwrap_or_default(),
    );
    let bodies = Rc::new(VecModel::from(
        world
            .balls
            .iter()
            .zip(TINTS.iter().cycle())
            .map(|(ball, tint)| ui::Body {
                x: ball.x,
                y: ball.y,
                size: 2.0 * ball.radius,
                tint: slint::Color::from_argb_encoded(*tint),
            })
            .collect::<Vec<_>>(),
    ));
    desktop.set_bodies(ModelRc::from(bodies.clone()));
    desktop.set_field_width(layout.field_width);
    desktop.set_field_height(layout.field_height);
    let simulation = Arc::new(Mutex::new(Simulation::default()));
    let shared = simulation.clone();
    thread::spawn(move || simulate(world, &shared));

    let mut surface = Surface {
        screen,
        pixels,
        stride: usize::try_from(dimension.width)?,
    };
    run(
        &mut surface,
        &window,
        &demo,
        &tasks,
        &bodies,
        &simulation,
        epoch_seconds,
    )
}

fn run(
    surface: &mut Surface<'_>,
    window: &MinimalSoftwareWindow,
    demo: &ui::Demo,
    tasks: &Mutex<VecDeque<Task>>,
    bodies: &VecModel<ui::Body>,
    simulation: &Mutex<Simulation>,
    epoch_seconds: Option<u64>,
) -> Result<(), Box<dyn Error>> {
    let start = Instant::now();
    let mut second = Second::default();
    let mut shown_second = 0;

    loop {
        let frame_start = Instant::now();
        let elapsed = start.elapsed().as_secs();
        if elapsed != shown_second {
            shown_second = elapsed;
            let (steps, busy) = show_second(demo, &second, simulation);
            second = Second {
                steps,
                busy,
                ..Second::default()
            };
        }
        demo.global::<ui::Desktop<'_>>()
            .set_clock(clock_text(epoch_seconds, elapsed).into());

        let pending = tasks
            .lock()
            .map(|mut queue| core::mem::take(&mut *queue))
            .unwrap_or_default();
        for task in pending {
            task();
        }
        move_bodies(bodies, simulation);

        slint::platform::update_timers_and_animations();

        let draw_start = Instant::now();
        let mut dirty = None;
        window.draw_if_needed(|renderer| {
            dirty = Some(renderer.render(surface.pixels, surface.stride));
        });
        if let Some(area) = dirty.as_ref().and_then(bounding_area) {
            surface
                .screen
                .present(area)
                .map_err(|error| format!("present refused: {error:?}"))?;
            second.frames = second.frames.saturating_add(1);
            second.drawing = second.drawing.saturating_add(draw_start.elapsed());
        }

        thread::sleep(FRAME_INTERVAL.saturating_sub(frame_start.elapsed()));
    }
}

fn move_bodies(bodies: &VecModel<ui::Body>, simulation: &Mutex<Simulation>) {
    let Ok(state) = simulation.lock() else {
        return;
    };
    for (row, &(x, y)) in state.centers.iter().enumerate() {
        if let Some(body) = bodies.row_data(row) {
            bodies.set_row_data(row, ui::Body { x, y, ..body });
        }
    }
}

fn show_second(
    demo: &ui::Demo,
    second: &Second,
    simulation: &Mutex<Simulation>,
) -> (u32, Duration) {
    let (steps, busy) = simulation
        .lock()
        .map(|state| (state.steps, state.busy))
        .unwrap_or_default();
    // Счётчики потока симуляции растут монотонно, секунда берёт их приращение.
    let step_count = steps.wrapping_sub(second.steps);
    let step_micros = busy
        .saturating_sub(second.busy)
        .as_micros()
        .checked_div(u128::from(step_count))
        .unwrap_or(0);
    demo.global::<ui::Desktop<'_>>().set_physics_note(
        format!("{BODY_COUNT} bodies, {step_count} steps/s, {step_micros} us per step").into(),
    );

    if second.frames > 0 {
        let frame_ms = second.drawing.as_secs_f64() * 1000.0 / f64::from(second.frames);
        demo.global::<ui::Desktop<'_>>()
            .set_frame_rate(format!("{} fps, {frame_ms:.1} ms per frame", second.frames).into());
    }
    (steps, busy)
}

fn simulate(mut world: physics::World, shared: &Mutex<Simulation>) {
    let dt = STEP_INTERVAL.as_secs_f32();
    loop {
        let started = Instant::now();
        world.step(dt);
        let busy = started.elapsed();

        let Ok(mut state) = shared.lock() else {
            return;
        };
        state.centers.clear();
        state
            .centers
            .extend(world.balls.iter().map(|ball| (ball.x, ball.y)));
        state.steps = state.steps.wrapping_add(1);
        state.busy = state.busy.saturating_add(busy);
        drop(state);

        thread::sleep(STEP_INTERVAL.saturating_sub(started.elapsed()));
    }
}

fn start_file_scan(demo: &ui::Demo) {
    let selection = Arc::new(Mutex::new(Selection {
        mount: "/".into(),
        folder: FOLDER.into(),
        file: FILE.into(),
    }));
    let (rescan, rescan_requests) = mpsc::channel();
    let desktop = demo.global::<ui::Desktop<'_>>();
    desktop.set_mount("/".into());
    desktop.set_folder(FOLDER.into());
    desktop.set_file(FILE.into());
    let weak = demo.as_weak();
    let chosen = selection.clone();
    desktop.on_select(move |mount, folder, file| {
        if let Some(demo) = weak.upgrade() {
            let desktop = demo.global::<ui::Desktop<'_>>();
            desktop.set_mount(mount.clone());
            desktop.set_folder(folder.clone());
            desktop.set_file(file.clone());
        }
        if let Ok(mut chosen) = chosen.lock() {
            *chosen = Selection {
                mount: mount.into(),
                folder: folder.into(),
                file: file.into(),
            };
        }
        let _ = rescan.send(());
    });

    let weak = demo.as_weak();
    thread::spawn(move || scan_files(&weak, &selection, &rescan_requests));
}

fn scan_files(
    demo: &slint::Weak<ui::Demo>,
    selection: &Mutex<Selection>,
    rescan_requests: &Receiver<()>,
) {
    loop {
        let chosen = match selection.lock() {
            Ok(chosen) => chosen.clone(),
            Err(_) => return,
        };
        let posted = match Scan::collect(&chosen) {
            Ok(scan) => demo.upgrade_in_event_loop(move |demo| scan.show(&demo)),
            Err(error) => {
                let line = text_line(&format!("unavailable: {error}"), 1);
                demo.upgrade_in_event_loop(move |demo| {
                    demo.global::<ui::Desktop<'_>>()
                        .set_file_lines(ModelRc::new(VecModel::from(vec![line])));
                })
            }
        };
        if posted.is_err() {
            return;
        }
        if rescan_requests.recv_timeout(RESCAN_INTERVAL) == Err(RecvTimeoutError::Disconnected) {
            return;
        }
    }
}

fn read_input(
    session: &input::Client<SessionTransport>,
    screen: events::Screen,
    demo: &slint::Weak<ui::Demo>,
) {
    let mut translator = events::Translator::new(screen);
    loop {
        let records = match session.next() {
            Ok(records) => records,
            Err(error) => {
                println!("input stopped: {error:?}");
                return;
            }
        };
        for record in records.records() {
            translator.feed(record, |device| session.describe(device).ok());
        }

        let events = translator.take_events();
        if events.is_empty() {
            continue;
        }
        let posted = demo.upgrade_in_event_loop(move |demo| {
            for event in events {
                demo.window().dispatch_event(event);
            }
        });
        if posted.is_err() {
            return;
        }
    }
}

fn entries(path: &str) -> io::Result<Vec<ui::Entry>> {
    let mut entries = fs::read_dir(path)?
        .map(|entry| {
            let entry = entry?;
            let metadata = entry.metadata()?;
            Ok(ui::Entry {
                name: entry.file_name().to_string_lossy().into_owned().into(),
                size: if metadata.is_dir() {
                    String::new()
                } else {
                    size_text(metadata.len())
                }
                .into(),
                folder: metadata.is_dir(),
            })
        })
        .collect::<io::Result<Vec<_>>>()?;
    // Каталоги идут первыми, внутри групп записи упорядочены по имени.
    entries.sort_by(|left, right| {
        right
            .folder
            .cmp(&left.folder)
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(entries)
}

fn child_path(parent: &str, name: &str) -> String {
    format!("{}/{name}", parent.trim_end_matches('/'))
}

fn preview(path: &str) -> io::Result<String> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(PREVIEW_BYTES_MAX)
        .read_to_end(&mut bytes)?;
    match String::from_utf8(bytes) {
        Ok(text) => Ok(text),
        Err(error) if error.utf8_error().error_len().is_none() => {
            let valid = error.utf8_error().valid_up_to();
            let mut bytes = error.into_bytes();
            bytes.truncate(valid);
            Ok(String::from_utf8(bytes).unwrap_or_default())
        }
        Err(_) => Ok("binary file".into()),
    }
}

fn text_line(line: &str, number: usize) -> ui::Line {
    ui::Line {
        number: number.to_string().into(),
        text: line.into(),
    }
}

fn size_text(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * KIB;

    if bytes < KIB {
        format!("{bytes} B")
    } else if bytes < MIB {
        format!("{}.{} KiB", bytes / KIB, bytes % KIB * 10 / KIB)
    } else {
        format!("{}.{} MiB", bytes / MIB, bytes % MIB * 10 / MIB)
    }
}

fn rtc_now(providers: &Providers<rtc::Protocol>) -> Result<Option<u64>, Box<dyn Error>> {
    let Some(found) = providers.all(Attributes::empty())?.next().transpose()? else {
        return Ok(None);
    };
    let seconds = providers
        .open(&found)?
        .client()
        .with_wait(CALL_TIMEOUT)
        .now()?;
    Ok(Some(seconds))
}

fn clock_text(epoch_seconds: Option<u64>, elapsed: u64) -> String {
    let Some(epoch_seconds) = epoch_seconds else {
        return format!(
            "up {:02}:{:02}:{:02}",
            elapsed / SECONDS_PER_HOUR,
            elapsed % SECONDS_PER_HOUR / 60,
            elapsed % 60
        );
    };
    let epoch_seconds = epoch_seconds.saturating_add(elapsed);
    let days = epoch_seconds / SECONDS_PER_DAY;
    let minutes = epoch_seconds % SECONDS_PER_DAY / 60;
    let (month, day) = civil_date(days);
    // 1 января 1970 года - четверг.
    let weekday = WEEKDAYS
        .get(usize::try_from((days + 4) % 7).unwrap_or(0))
        .unwrap_or(&"");
    let month = MONTHS
        .get(usize::try_from(month - 1).unwrap_or(0))
        .unwrap_or(&"");
    format!(
        "{weekday} {day} {month}  {:02}:{:02} UTC",
        minutes / 60,
        minutes % 60
    )
}

fn civil_date(days: u64) -> (u64, u64) {
    // Алгоритм `civil_from_days` Говарда Хиннанта, дни отсчитываются от 1 января 1970 года.
    let day_of_era = days.saturating_add(719_468) % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    (month, day)
}

fn surface_pixels(
    mapping: &mut kernel_objects::Mapping,
    dimension: DisplayDimension,
) -> Result<&mut [Xrgb], Box<dyn Error>> {
    let needed = dimension.surface_size().ok_or("surface size overflows")?;
    if mapping.size_bytes() < needed {
        return Err("surface is smaller than the screen".into());
    }
    let count = usize::try_from(u64::from(dimension.width) * u64::from(dimension.height))?;
    let base = usize::try_from(mapping.va())?;

    #[allow(unsafe_code)]
    // SAFETY: `mapping` - RW-отображение размером не меньше `count * 4` байт, выровненное по
    // странице. Срез заимствует `mapping` исключительно, поэтому отображение не снимается и не
    // читается в обход среза, пока срез жив. `Xrgb` - `repr(transparent)` над `u32`, любое
    // значение допустимо.
    let pixels = unsafe { core::slice::from_raw_parts_mut(base as *mut Xrgb, count) };
    Ok(pixels)
}

fn bounding_area(region: &PhysicalRegion) -> Option<Area> {
    let origin = region.bounding_box_origin();
    let size = region.bounding_box_size();
    if size.width == 0 || size.height == 0 {
        return None;
    }
    Some(Area {
        x: u32::try_from(origin.x).ok()?,
        y: u32::try_from(origin.y).ok()?,
        width: size.width,
        height: size.height,
    })
}
