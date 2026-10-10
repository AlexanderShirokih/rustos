//! Сервер `Display` для тестов. Экран - анонимный регион. Бюджета памяти хватает на одну
//! поверхность.

#![no_std]
#![no_main]

extern crate alloc;

mod display_fixture_protocol;
mod failed;

use alloc::{boxed::Box, vec::Vec};

use component_startup::{
    Requirements as _, SessionEvent, SessionEvents, component_manifest, serve_export, whole_export,
};
use display_abi::{
    Area, DisplayDimension, DisplayError, PhysicalSize, SURFACE_PIXEL_BYTES, display,
};
use display_fixture_protocol::{DIMENSION, PHYSICAL_SIZE};
use failed::Failed;
use ipc::{IntoWireValue, wire::Cap};
use kernel_objects::{Delivery, Mapping, MemoryRegion, Port, Resource};
use runtime::{FAILED_EXIT_CODE, MemoryAccess, UserMemFlags, exit, log_fmt, log_line, spawn_owned};
use syscall::{Handle, Rights, kind};

const ROW_MAX_BYTES: usize = DIMENSION.width as usize * SURFACE_PIXEL_BYTES as usize;

component_manifest! {
    struct DisplayStartup {
        exports: [display::Protocol => [{ kind = "fixture" }]],
    }
}

struct Surface {
    badge: u64,
    mapping: Mapping,
}

struct Screen {
    pixels: Mapping,
    surface_size: u64,
    surfaces: Vec<Surface>,
    budget: Resource,
    _origin: Resource,
}

impl Screen {
    fn new() -> Option<Self> {
        let surface_size = DIMENSION.surface_size()?;
        let origin = Resource::self_resource().ok()?;
        let budget = origin.split(surface_size).ok()?;

        let region = origin.create_virtual(surface_size, MemoryAccess::RW).ok()?;
        let pixels = Mapping::new(region, surface_size, UserMemFlags::ReadWrite).ok()?;

        // Бюджет `budget` действует, пока открыт `origin`.
        Some(Self {
            pixels,
            surface_size,
            surfaces: Vec::new(),
            budget,
            _origin: origin,
        })
    }

    fn acquire(&mut self, badge: u64) -> Result<MemoryRegion, DisplayError> {
        if self.surfaces.iter().any(|surface| surface.badge == badge) {
            return Err(DisplayError::AlreadyAcquired);
        }

        let region = self
            .budget
            .create_virtual(self.surface_size, MemoryAccess::RW)
            .map_err(|_| DisplayError::NoSpace)?;
        let mapping = Mapping::new(region, self.surface_size, UserMemFlags::ReadWrite)
            .map_err(|_| DisplayError::NoSpace)?;

        // Закрытие региона отображения в `close` отзывает эту копию у клиента.
        let granted = mapping
            .region()
            .duplicate(Rights::READ | Rights::WRITE | Rights::TRANSFER, None)
            .map_err(|_| DisplayError::NoSpace)?;

        self.surfaces.push(Surface { badge, mapping });
        Ok(granted)
    }

    fn copy(&self, badge: u64, area: Area) -> Result<(), DisplayError> {
        let surface = self
            .surfaces
            .iter()
            .find(|surface| surface.badge == badge)
            .ok_or(DisplayError::NoSurface)?;
        DIMENSION.check_area(area)?;

        let mut row = [0u8; ROW_MAX_BYTES];
        let row = usize::try_from(u64::from(area.width) * u64::from(SURFACE_PIXEL_BYTES))
            .ok()
            .and_then(|len| row.get_mut(..len))
            .ok_or(DisplayError::OutOfBounds)?;

        for y in area.y..area.y + area.height {
            let offset = usize::try_from(
                u64::from(y) * DIMENSION.surface_stride()
                    + u64::from(area.x) * u64::from(SURFACE_PIXEL_BYTES),
            )
            .map_err(|_| DisplayError::OutOfBounds)?;

            surface
                .mapping
                .copy_out_at(offset, row)
                .map_err(|_| DisplayError::OutOfBounds)?;
            self.pixels
                .copy_in_at(offset, row)
                .map_err(|_| DisplayError::OutOfBounds)?;
        }

        Ok(())
    }

    fn close(&mut self, badge: u64) {
        self.surfaces.retain(|surface| surface.badge != badge);
    }
}

impl display::Service<Delivery> for Screen {
    fn dimension(&mut self, _frame: &Delivery) -> DisplayDimension {
        DIMENSION
    }

    fn physical_size(&mut self, _frame: &Delivery) -> Option<PhysicalSize> {
        Some(PHYSICAL_SIZE)
    }

    fn acquire_surface(
        &mut self,
        frame: &Delivery,
    ) -> impl IntoWireValue<Delivery, Read = Result<Cap<kind::MemoryRegion>, DisplayError>> {
        self.acquire(frame.badge)
    }

    fn present(&mut self, frame: &Delivery, area: Area) -> Result<(), DisplayError> {
        self.copy(frame.badge, area)
    }
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Failed> {
    let service = Port::create().map_err(|_| Failed)?;
    let mut screen = Screen::new().ok_or(Failed)?;
    let service: &'static Port = Box::leak(Box::new(service));
    let parent_port = DisplayStartup::from_parent(parent).parent;
    let export_thread = spawn_owned(move || {
        let failure = serve_export::<display::Protocol>(&parent_port, whole_export(service));
        log_fmt(format_args!("export stopped: {failure:?}"));
        exit(FAILED_EXIT_CODE)
    });
    if export_thread.is_err() {
        return Err(Failed);
    }

    log_line("started");
    serve(service, &mut screen);
    Err(Failed)
}

fn serve(service: &Port, screen: &mut Screen) {
    let mut events = SessionEvents::new(service);

    while let Some(event) = events.recv() {
        match event {
            SessionEvent::Frame(frame) => {
                let _ = display::dispatch(screen, frame);
            }
            SessionEvent::Gone(badge) => screen.close(badge),
        }
    }
}
