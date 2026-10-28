//! Сервер `Display` для тестов. Экран - анонимный регион. Бюджета памяти хватает на одну
//! поверхность.

#![no_std]
#![no_main]

extern crate alloc;

mod display_fixture_protocol;
mod failed;

use alloc::{rc::Rc, vec::Vec};
use core::cell::RefCell;

use component_startup::{
    AsyncStartup, Requirements as _, SessionPort, component_manifest, serve_component, whole_export,
};
use display_abi::{
    Area, DisplayDimension, DisplayError, PhysicalSize, SURFACE_PIXEL_BYTES, display,
};
use display_fixture_protocol::{DIMENSION, PHYSICAL_SIZE};
use executor::{Executor, PortChannel};
use failed::Failed;
use ipc::{IntoWireValue, wire::Cap};
use kernel_objects::{Delivery, Mapping, MemoryRegion, Resource};
use runtime::{MemoryAccess, UserMemFlags, log_line};
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
    surfaces: RefCell<Vec<Surface>>,
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
            surfaces: RefCell::new(Vec::new()),
            budget,
            _origin: origin,
        })
    }

    fn acquire(&self, badge: u64) -> Result<MemoryRegion, DisplayError> {
        if self
            .surfaces
            .borrow()
            .iter()
            .any(|surface| surface.badge == badge)
        {
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

        self.surfaces.borrow_mut().push(Surface { badge, mapping });
        Ok(granted)
    }

    fn copy(&self, badge: u64, area: Area) -> Result<(), DisplayError> {
        let surfaces = self.surfaces.borrow();
        let surface = surfaces
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

    fn close(&self, badge: u64) {
        self.surfaces
            .borrow_mut()
            .retain(|surface| surface.badge != badge);
    }
}

#[allow(clippy::unused_async_trait_impl)]
impl display::Service<Delivery> for Screen {
    fn dimension(&self, _frame: &Delivery) -> DisplayDimension {
        DIMENSION
    }

    fn physical_size(&self, _frame: &Delivery) -> Option<PhysicalSize> {
        Some(PHYSICAL_SIZE)
    }

    fn acquire_surface(
        &self,
        frame: &Delivery,
    ) -> impl IntoWireValue<Delivery, Read = Result<Cap<kind::MemoryRegion>, DisplayError>> {
        self.acquire(frame.badge)
    }

    async fn present(&self, frame: &Delivery, area: Area) -> Result<(), DisplayError> {
        self.copy(frame.badge, area)
    }
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Failed> {
    let executor = Rc::new(Executor::new().map_err(|_| Failed)?);
    let service = Rc::new(SessionPort::create().map_err(|_| Failed)?);
    let screen = Rc::new(Screen::new().ok_or(Failed)?);
    let parent = DisplayStartup::from_parent(parent).parent;
    let parent = AsyncStartup::new(PortChannel::from_transport(parent.into_channel()));
    let calls = screen.clone();
    let opened = service.clone();

    let served = serve_component::<display::Protocol, _>(
        &executor,
        parent,
        service,
        move |frame| display::serve(&calls, frame),
        whole_export(opened),
        move |badge| screen.close(badge),
    );

    log_line("started");
    let _ = executor.block_on(served);
    Err(Failed)
}
