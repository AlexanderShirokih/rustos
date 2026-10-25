//! Окно Slint на весь экран сессии `Display` и цикл кадра.

use core::{convert::Infallible, error::Error, fmt, time::Duration};
use std::{rc::Rc, time::Instant};

use display_abi::{Area, DisplayDimension, DisplayError, display};
use executor::{PortChannel, Reactor, TaskHandle};
use ipc::CallError;
use kernel_objects::{Mapping, MemoryRegion, Session, SessionTransport};
use slint::{
    LogicalSize, PhysicalSize,
    platform::{
        WindowAdapter, WindowEvent,
        software_renderer::{MinimalSoftwareWindow, PhysicalRegion, RepaintBufferType},
    },
};
use syscall::{Timeout, UserMemFlags};

use crate::{
    event_loop::{EventLoop, TaskQueue},
    pixel::Xrgb,
    scale::scale_factor,
};

const FRAME_INTERVAL: Duration = Duration::from_millis(16);

/// Сессия `Display` с отображённой поверхностью и окно Slint на весь экран.
pub struct ScreenPlatform {
    attached: Option<Attached>,
    dimension: DisplayDimension,
    scale_factor: f32,
    stride: usize,
    window: Rc<MinimalSoftwareWindow>,
    pub(crate) tasks: TaskQueue,
    blank: bool,
    reactor: Reactor,
    pub(crate) input: Option<TaskHandle>,
}

struct Attached {
    surface: Mapping,
    screen: display::Client<PortChannel<SessionTransport>>,
}

/// Отказ платформы Slint над сессией `Display`.
#[derive(Debug)]
pub enum ScreenError {
    /// Сессия `Display` отказала в операции.
    Display(CallError<DisplayError>),
    /// Поверхность не отображается целиком либо у Slint уже есть платформа.
    Setup,
}

impl ScreenPlatform {
    /// Получает размер, физический размер и поверхность сессии `screen` и устанавливает
    /// платформу Slint, окно которой занимает весь экран. Масштаб окна выбирается по плотности
    /// экрана.
    pub async fn open(
        screen: Session<display::Protocol>,
        reactor: Reactor,
    ) -> Result<Self, ScreenError> {
        let screen = PortChannel::client(screen, reactor.clone());
        let dimension = screen
            .dimension()
            .await
            .map_err(|error| ScreenError::Display(error.into()))?;

        let physical_size = screen
            .physical_size()
            .await
            .map_err(|error| ScreenError::Display(error.into()))?;

        let surface = map_surface(&screen, dimension).await?;
        let stride = usize::try_from(dimension.width).map_err(|_| ScreenError::Setup)?;

        let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
        let tasks = TaskQueue::default();
        slint::platform::set_platform(Box::new(EventLoop::new(window.clone(), tasks.clone())))
            .map_err(|_| ScreenError::Setup)?;
        window.set_size(PhysicalSize::new(dimension.width, dimension.height));
        let scale = scale_factor(dimension, physical_size);
        window.dispatch_event(WindowEvent::ScaleFactorChanged {
            scale_factor: scale,
        });

        Ok(Self {
            attached: Some(Attached { surface, screen }),
            dimension,
            scale_factor: scale,
            stride,
            window,
            tasks,
            blank: false,
            reactor,
            input: None,
        })
    }

    /// Снимает отображение поверхности и закрывает сессию экрана. Пока экран отпущен, цикл
    /// кадра выполняет задачи, таймеры и анимации Slint и не рисует окно.
    pub fn release(&mut self) {
        self.attached = None;
    }

    /// Подключает сессию `screen` вместо прежней. Размер экрана сессии должен совпадать с
    /// размером экрана [`open`](Self::open). Следующий кадр рисует окно целиком.
    pub async fn attach(&mut self, screen: Session<display::Protocol>) -> Result<(), ScreenError> {
        let screen = PortChannel::client(screen, self.reactor.clone());
        self.attached = None;
        let dimension = screen
            .dimension()
            .await
            .map_err(|error| ScreenError::Display(error.into()))?;

        if dimension != self.dimension {
            return Err(ScreenError::Setup);
        }

        let surface = map_surface(&screen, dimension).await?;

        self.attached = Some(Attached { surface, screen });
        // Поверхность новой сессии не содержит окна, и следующий кадр рисует его целиком.
        self.blank = true;
        self.window.window().request_redraw();
        Ok(())
    }

    /// Размер экрана в пикселях.
    pub fn dimension(&self) -> DisplayDimension {
        self.dimension
    }

    /// Число физических пикселей на логический, выбранное по плотности экрана.
    pub fn scale_factor(&self) -> f32 {
        self.scale_factor
    }

    /// Размер окна в логических пикселях.
    pub fn logical_size(&self) -> LogicalSize {
        PhysicalSize::new(self.dimension.width, self.dimension.height).to_logical(self.scale_factor)
    }

    /// Выполняет цикл кадра задачей executor. Кадр выполняет задачи очереди, продвигает таймеры
    /// и анимации Slint, рисует окно и переносит изменённую область на экран. Следующий кадр
    /// начинается, когда в очереди появилась задача либо наступил срок таймера Slint, а пока у
    /// окна идёт анимация - не позже конца интервала кадра. Цикл заканчивается только отказом
    /// переноса.
    pub async fn run(&mut self) -> Result<Infallible, ScreenError> {
        self.run_with(|_| {}).await
    }

    /// Крутит цикл кадра, как [`run`](Self::run), пока `done` после задач кадра не вернёт
    /// значение, и возвращает его.
    pub async fn run_until<T>(
        &mut self,
        mut done: impl FnMut() -> Option<T>,
    ) -> Result<T, ScreenError> {
        self.run_frames(|_| done()).await
    }

    /// Крутит цикл кадра, как [`run`](Self::run), и в начале кадра вызывает `frame` со временем
    /// рисования и переноса предыдущего кадра, если тот изменил экран.
    pub async fn run_with(
        &mut self,
        mut frame: impl FnMut(Option<Duration>),
    ) -> Result<Infallible, ScreenError> {
        self.run_frames(|drawn| {
            frame(drawn);
            None
        })
        .await
    }

    async fn run_frames<T>(
        &mut self,
        mut frame: impl FnMut(Option<Duration>) -> Option<T>,
    ) -> Result<T, ScreenError> {
        let mut drawn = None;
        loop {
            let frame_start = Instant::now();
            let revision = self.tasks.run_pending();
            if let Some(done) = frame(drawn.take()) {
                return Ok(done);
            }
            slint::platform::update_timers_and_animations();

            let draw_start = Instant::now();
            if let Some(area) = self.draw() {
                if let Some(attached) = &self.attached
                    && let Err(error) = attached.screen.present(area).await
                {
                    return Err(ScreenError::Display(error));
                }
                drawn = Some(draw_start.elapsed());
            }

            let timer = slint::platform::duration_until_next_timer_update();
            let timeout = if self.window.has_active_animations() {
                let frame_end = FRAME_INTERVAL.saturating_sub(frame_start.elapsed());
                Some(timer.map_or(frame_end, |timer| timer.min(frame_end)))
            } else {
                timer
            };

            let changed = self.tasks.changed(revision);
            if let Some(timeout) = timeout {
                let nanos = u64::try_from(timeout.as_nanos()).unwrap_or(u64::MAX);
                let _ = self.reactor.timeout(Timeout::from_ns(nanos), changed).await;
            } else {
                changed.await;
            }
        }
    }

    fn draw(&mut self) -> Option<Area> {
        let attached = self.attached.as_mut()?;
        let pixels = surface_pixels(&mut attached.surface, self.dimension)?;
        let blank = &mut self.blank;
        let mut dirty = None;
        self.window.draw_if_needed(|renderer| {
            if core::mem::take(blank) {
                renderer.set_repaint_buffer_type(RepaintBufferType::NewBuffer);
            }
            dirty = Some(renderer.render(pixels, self.stride));
            renderer.set_repaint_buffer_type(RepaintBufferType::ReusedBuffer);
        });
        dirty.as_ref().and_then(bounding_area)
    }
}

impl fmt::Display for ScreenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Display(error) => write!(f, "display refused: {error:?}"),
            Self::Setup => f.write_str("screen setup failed"),
        }
    }
}

impl Error for ScreenError {}

async fn map_surface(
    screen: &display::Client<PortChannel<SessionTransport>>,
    dimension: DisplayDimension,
) -> Result<Mapping, ScreenError> {
    let region = screen
        .acquire_surface()
        .await
        .map_err(ScreenError::Display)?;

    let mut surface = Mapping::full(MemoryRegion::from(region), UserMemFlags::ReadWrite)
        .map_err(|_| ScreenError::Setup)?;

    if surface_pixels(&mut surface, dimension).is_none() {
        return Err(ScreenError::Setup);
    }

    Ok(surface)
}

fn surface_pixels(surface: &mut Mapping, dimension: DisplayDimension) -> Option<&mut [Xrgb]> {
    if surface.size_bytes() < dimension.surface_size()? {
        return None;
    }
    let count = usize::try_from(u64::from(dimension.width) * u64::from(dimension.height)).ok()?;
    let base = usize::try_from(surface.va()).ok()?;

    #[allow(unsafe_code)]
    // SAFETY: `surface` - отображение `Mapping::full` с `UserMemFlags::ReadWrite` из
    // `map_surface`. Ядро инициализировало его память, оно владеет регионом, выровнено
    // по странице и, по проверке выше, не меньше `count * 4` байт. Срез заимствует `surface`
    // исключительно, поэтому отображение не снимается, пока срез жив. Поставщик `Display`
    // читает поверхность только внутри `present`, который отвечает после записи на экран, а
    // срез снимается до `present`. `Xrgb` - `repr(transparent)` над `u32`, любое значение
    // допустимо.
    let pixels = unsafe { core::slice::from_raw_parts_mut(base as *mut Xrgb, count) };
    Some(pixels)
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
