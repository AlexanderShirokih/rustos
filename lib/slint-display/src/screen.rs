//! Окно Slint на весь экран сессии `Display` и цикл кадра.

use core::{error::Error, fmt, time::Duration};
use std::{rc::Rc, time::Instant};

use display_abi::{Area, DisplayDimension, DisplayError, display};
use ipc::CallError;
use kernel_objects::{Mapping, MemoryRegion, SessionTransport};
use slint::{
    PhysicalSize,
    platform::{
        WindowEvent,
        software_renderer::{MinimalSoftwareWindow, PhysicalRegion, RepaintBufferType},
    },
};
use syscall::UserMemFlags;

use crate::{
    event_loop::{EventLoop, TaskQueue},
    pixel::Xrgb,
};

const FRAME_INTERVAL: Duration = Duration::from_millis(16);

/// Сессия `Display` с отображённой поверхностью и окно Slint на весь экран.
pub struct ScreenPlatform {
    screen: display::Client<SessionTransport>,
    surface: Mapping,
    dimension: DisplayDimension,
    physical_size: Option<display_abi::PhysicalSize>,
    stride: usize,
    window: Rc<MinimalSoftwareWindow>,
    tasks: TaskQueue,
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
    /// платформу Slint, окно которой занимает весь экран.
    pub fn open(screen: display::Client<SessionTransport>) -> Result<Self, ScreenError> {
        let dimension = screen
            .dimension()
            .map_err(|error| ScreenError::Display(error.into()))?;
        let physical_size = screen
            .physical_size()
            .map_err(|error| ScreenError::Display(error.into()))?;
        let region = MemoryRegion::from(screen.acquire_surface().map_err(ScreenError::Display)?);
        let mut surface =
            Mapping::full(region, UserMemFlags::ReadWrite).map_err(|_| ScreenError::Setup)?;
        if surface_pixels(&mut surface, dimension).is_none() {
            return Err(ScreenError::Setup);
        }
        let stride = usize::try_from(dimension.width).map_err(|_| ScreenError::Setup)?;

        let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
        let tasks = TaskQueue::default();
        slint::platform::set_platform(Box::new(EventLoop::new(window.clone(), tasks.clone())))
            .map_err(|_| ScreenError::Setup)?;
        window.set_size(PhysicalSize::new(dimension.width, dimension.height));
        Ok(Self {
            screen,
            surface,
            dimension,
            physical_size,
            stride,
            window,
            tasks,
        })
    }

    /// Размер экрана в пикселях.
    pub fn dimension(&self) -> DisplayDimension {
        self.dimension
    }

    /// Физический размер экрана. `None`, если устройство его не сообщает.
    pub fn physical_size(&self) -> Option<display_abi::PhysicalSize> {
        self.physical_size
    }

    /// Задаёт окну число физических пикселей на логический.
    pub fn set_scale_factor(&self, scale_factor: f32) {
        self.window
            .dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor });
    }

    /// Крутит цикл кадра в вызвавшем потоке. Кадр выполняет задачи очереди, вызывает `frame`,
    /// продвигает таймеры и анимации Slint, рисует окно и переносит изменённую область на
    /// экран. `frame` получает время рисования и переноса предыдущего кадра, если тот изменил
    /// экран. Следующий кадр начинается, когда в очереди появилась задача либо наступил срок
    /// таймера Slint, а пока у окна идёт анимация - не позже конца интервала кадра. Цикл
    /// заканчивается только отказом переноса.
    pub fn run(&mut self, mut frame: impl FnMut(Option<Duration>)) -> ScreenError {
        let mut drawn = None;
        loop {
            let frame_start = Instant::now();
            self.tasks.run_pending();
            frame(drawn.take());
            slint::platform::update_timers_and_animations();

            let draw_start = Instant::now();
            if let Some(area) = self.draw() {
                if let Err(error) = self.screen.present(area) {
                    return ScreenError::Display(error);
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

            self.tasks.wait_for_task(timeout);
        }
    }

    fn draw(&mut self) -> Option<Area> {
        let pixels = surface_pixels(&mut self.surface, self.dimension)?;
        let mut dirty = None;
        self.window.draw_if_needed(|renderer| {
            dirty = Some(renderer.render(pixels, self.stride));
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

fn surface_pixels(surface: &mut Mapping, dimension: DisplayDimension) -> Option<&mut [Xrgb]> {
    if surface.size_bytes() < dimension.surface_size()? {
        return None;
    }
    let count = usize::try_from(u64::from(dimension.width) * u64::from(dimension.height)).ok()?;
    let base = usize::try_from(surface.va()).ok()?;

    #[allow(unsafe_code)]
    // SAFETY: `surface` - отображение `Mapping::full` с `UserMemFlags::ReadWrite` из
    // `ScreenPlatform::open`. Ядро инициализировало его память, оно владеет регионом, выровнено
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
