//! Экран протокола `Display` для кадра движка.
//!
//! Кадр движка растягивается до 4:3 и выводится по центру экрана, повёрнутым на 90 градусов по
//! часовой стрелке, если так он крупнее. Отказ вывода пишется в журнал один раз.

use alloc::{vec, vec::Vec};
use core::fmt;

use display_abi::{Area, DisplayDimension, DisplayError, SURFACE_PIXEL_BYTES, display};
use ipc::{CallError, wire::IpcError};
use kernel_objects::{Mapping, MemoryRegion, Session, SessionTransport};
use runtime::{Mutex, UserMemFlags, log_fmt};

use crate::frame::{FRAME_HEIGHT, FRAME_WIDTH};

pub(crate) const FRAME_PIXELS: usize = FRAME_WIDTH as usize * FRAME_HEIGHT as usize;

const PIXEL_BYTES: usize = SURFACE_PIXEL_BYTES as usize;

const ASPECT_WIDTH: u32 = 4;
const ASPECT_HEIGHT: u32 = 3;

static SCREEN: Mutex<Option<Screen>> = Mutex::new(None);

#[derive(Debug)]
pub(crate) enum ScreenError {
    Dimension(IpcError),
    TooSmall(DisplayDimension),
    Surface(CallError<DisplayError>),
    Mapping(kernel_objects::Error),
}

pub(crate) struct Screen {
    client: display::Client<SessionTransport>,
    surface: Mapping,
    area: Area,
    stride: usize,
    line_offsets: Vec<usize>,
    pixel_offsets: Vec<usize>,
    row: Vec<u8>,
    failure_logged: bool,
}

impl fmt::Display for ScreenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dimension(error) => write!(formatter, "screen dimension refused: {error:?}"),
            Self::TooSmall(screen) => write!(
                formatter,
                "screen {}x{} is too small for the {FRAME_WIDTH}x{FRAME_HEIGHT} frame",
                screen.width, screen.height
            ),
            Self::Surface(error) => write!(formatter, "screen surface refused: {error:?}"),
            Self::Mapping(error) => write!(formatter, "screen surface not mapped: {error:?}"),
        }
    }
}

impl Screen {
    pub(crate) fn open(session: Session<display::Protocol>) -> Result<Self, ScreenError> {
        let client = session.client();
        let dimension = client.dimension().map_err(ScreenError::Dimension)?;

        let upright = (dimension.width / ASPECT_WIDTH).min(dimension.height / ASPECT_HEIGHT);
        let sideways = (dimension.width / ASPECT_HEIGHT).min(dimension.height / ASPECT_WIDTH);
        let rotated = sideways > upright;
        let unit = upright.max(sideways);

        if unit == 0 {
            return Err(ScreenError::TooSmall(dimension));
        }

        let frame_width = FRAME_WIDTH as usize;
        let frame_height = FRAME_HEIGHT as usize;
        let (width, height, line_offsets, pixel_offsets) = if rotated {
            let (width, height) = (ASPECT_HEIGHT * unit, ASPECT_WIDTH * unit);
            (
                width,
                height,
                nearest_indices(height, frame_width).collect(),
                nearest_indices(width, frame_height)
                    .map(|line| (frame_height - 1 - line) * frame_width)
                    .collect(),
            )
        } else {
            let (width, height) = (ASPECT_WIDTH * unit, ASPECT_HEIGHT * unit);
            (
                width,
                height,
                nearest_indices(height, frame_height)
                    .map(|line| line * frame_width)
                    .collect(),
                nearest_indices(width, frame_width).collect(),
            )
        };
        let area = Area {
            x: (dimension.width - width) / 2,
            y: (dimension.height - height) / 2,
            width,
            height,
        };
        let region = client
            .acquire_surface()
            .map(MemoryRegion::from)
            .map_err(ScreenError::Surface)?;
        let surface = Mapping::full(region, UserMemFlags::ReadWrite)
            .map_err(|refused| ScreenError::Mapping(refused.error))?;

        let stride = dimension.width as usize * PIXEL_BYTES;
        let mut screen = Self {
            client,
            surface,
            area,
            stride,
            line_offsets,
            pixel_offsets,
            row: vec![0; width as usize * PIXEL_BYTES],
            failure_logged: false,
        };
        let cleared = screen
            .surface
            .zero_at(0, stride * dimension.height as usize);
        screen.show(
            cleared,
            Area {
                x: 0,
                y: 0,
                width: dimension.width,
                height: dimension.height,
            },
        );
        Ok(screen)
    }

    fn draw(&mut self, frame: &[u32]) {
        let copied = self.copy(frame);
        self.show(copied, self.area);
    }

    fn show(&mut self, written: kernel_objects::Result<()>, area: Area) {
        if let Err(error) = written {
            self.report(format_args!("frame not copied: {error:?}"));
        } else if let Err(error) = self.client.present(area) {
            self.report(format_args!("frame not presented: {error:?}"));
        }
    }

    fn report(&mut self, failure: fmt::Arguments<'_>) {
        if !self.failure_logged {
            log_fmt(failure);
            self.failure_logged = true;
        }
    }

    fn copy(&mut self, frame: &[u32]) -> kernel_objects::Result<()> {
        let left = self.area.x as usize * PIXEL_BYTES;
        let top = self.area.y as usize;
        let mut filled = None;

        for (line, &line_offset) in self.line_offsets.iter().enumerate() {
            if filled != Some(line_offset) {
                let targets = self.row.as_chunks_mut::<PIXEL_BYTES>().0;
                for (target, &pixel_offset) in targets.iter_mut().zip(&self.pixel_offsets) {
                    let pixel = frame.get(line_offset + pixel_offset).copied().unwrap_or(0);
                    *target = pixel.to_le_bytes();
                }
                filled = Some(line_offset);
            }
            self.surface
                .copy_in_at((top + line) * self.stride + left, &self.row)?;
        }
        Ok(())
    }
}

pub(crate) fn install(screen: Screen) {
    *SCREEN.lock() = Some(screen);
}

pub(crate) fn draw(frame: &[u32]) {
    if let Some(screen) = SCREEN.lock().as_mut() {
        screen.draw(frame);
    }
}

fn nearest_indices(target: u32, source: usize) -> impl Iterator<Item = usize> {
    let target = target as usize;
    (0..target).map(move |index| index * source / target)
}
