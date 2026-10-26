//! Экран протокола `Display` для кадра движка.
//!
//! Кадр движка растягивается до 4:3 и выводится по центру экрана, повёрнутым на 90 градусов по
//! часовой стрелке, если так он крупнее. Кнопки контроллера рисуются после
//! кадра и выделяются по состоянию ввода. Отказ вывода пишется в журнал один раз.

use alloc::{vec, vec::Vec};
use core::fmt;

use display_abi::{Area, DisplayDimension, DisplayError, SURFACE_PIXEL_BYTES, display};
use doomgeneric::{Control, Controls, Layout, Point};
use ipc::{CallError, wire::IpcError};
use kernel_objects::{Mapping, MemoryRegion, Session, SessionTransport};
use runtime::{Mutex, UserMemFlags, log_fmt};

use crate::frame::{FRAME_HEIGHT, FRAME_WIDTH};

pub(crate) const FRAME_PIXELS: usize = FRAME_WIDTH as usize * FRAME_HEIGHT as usize;

const PIXEL_BYTES: usize = SURFACE_PIXEL_BYTES as usize;

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
    layout: Layout,
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
        let dimension = client.dimension().wait().map_err(ScreenError::Dimension)?;

        let layout = Layout::new(dimension).ok_or(ScreenError::TooSmall(dimension))?;
        let area = layout.frame();
        let frame_width = FRAME_WIDTH as usize;
        let frame_height = FRAME_HEIGHT as usize;
        let (width, line_offsets, pixel_offsets) = if layout.is_rotated() {
            let (width, height) = (area.width, area.height);
            (
                width,
                nearest_indices(height, frame_width).collect(),
                nearest_indices(width, frame_height)
                    .map(|line| (frame_height - 1 - line) * frame_width)
                    .collect(),
            )
        } else {
            let (width, height) = (area.width, area.height);
            (
                width,
                nearest_indices(height, frame_height)
                    .map(|line| line * frame_width)
                    .collect(),
                nearest_indices(width, frame_width).collect(),
            )
        };
        let region = client
            .acquire_surface()
            .wait()
            .map(MemoryRegion::from)
            .map_err(ScreenError::Surface)?;

        let surface = Mapping::full(region, UserMemFlags::ReadWrite)
            .map_err(|refused| ScreenError::Mapping(refused.error))?;

        let stride = dimension.width as usize * PIXEL_BYTES;
        let mut screen = Self {
            client,
            surface,
            layout,
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

    pub(crate) fn layout(&self) -> Layout {
        self.layout
    }

    fn draw(&mut self, frame: &[u32], controls: &Controls) {
        let copied = self
            .surface
            .zero_at(0, self.stride * self.layout.panel().height as usize)
            .and_then(|()| self.copy(frame))
            .and_then(|()| {
                if controls.touch {
                    self.controls(controls)
                } else {
                    Ok(())
                }
            });
        let panel = self.layout.panel();
        self.show(
            copied,
            Area {
                x: 0,
                y: 0,
                width: panel.width,
                height: panel.height,
            },
        );
    }

    fn controls(&mut self, controls: &Controls) -> kernel_objects::Result<()> {
        for button in self.layout.buttons(controls.mode, controls.weapons) {
            if button.control == Control::Turn {
                continue;
            }
            let area = self.layout.physical_area(button.area);
            let active = button.active(&controls.held, controls.mode);
            let knob = crate::paint::knob(button, controls.stick);
            self.row.resize(area.width as usize * PIXEL_BYTES, 0);
            for y in area.y..area.y + area.height {
                self.surface.copy_out_at(
                    y as usize * self.stride + area.x as usize * PIXEL_BYTES,
                    &mut self.row,
                )?;
                for (x, pixel) in
                    (area.x..area.x + area.width).zip(self.row.as_chunks_mut::<PIXEL_BYTES>().0)
                {
                    let point = self.layout.logical(Point { x, y });
                    if let Some(color) = crate::paint::pixel(button, point, active, knob) {
                        *pixel = color.to_le_bytes();
                    }
                }
                self.surface.copy_in_at(
                    y as usize * self.stride + area.x as usize * PIXEL_BYTES,
                    &self.row,
                )?;
            }
        }
        Ok(())
    }

    fn show(&mut self, written: kernel_objects::Result<()>, area: Area) {
        if let Err(error) = written {
            self.report(format_args!("frame not copied: {error:?}"));
        } else if let Err(error) = self.client.present(area).wait() {
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
        let left = self.layout.frame().x as usize * PIXEL_BYTES;
        let top = self.layout.frame().y as usize;
        self.row
            .resize(self.layout.frame().width as usize * PIXEL_BYTES, 0);
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

pub(crate) fn draw(frame: &[u32], controls: &Controls) {
    if let Some(screen) = SCREEN.lock().as_mut() {
        screen.draw(frame, controls);
    }
}

fn nearest_indices(target: u32, source: usize) -> impl Iterator<Item = usize> {
    let target = target as usize;
    (0..target).map(move |index| index * source / target)
}
