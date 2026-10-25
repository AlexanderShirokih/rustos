//! Экраны машины: каждый экспорт `Display`, найденный перебором при запущенной `Platform`.

use alloc::vec::Vec;

use component_abi::RequirementDeclaration;
use component_startup::Found;
use display_abi::{Area, DisplayDimension, DisplayError, SURFACE_PIXEL_BYTES, display};
use ipc::{CallError, Protocol};
use kernel_objects::{Mapping, MemoryRegion, SessionTransport};
use runtime::{Timeout, UserMemFlags};
use userland::USERLAND_PAGE_SIZE;

use crate::{
    composition::Importer,
    rootkeeper_system::{finish, with_drivers},
};

const DISPLAY_REQUIREMENT: &str = "displays";

const SCREEN_REPLY_TIMEOUT: Timeout = Timeout::from_secs(2);

const SURFACE_CHANNEL_MAX: u64 = 0xFF;
const SURFACE_RED_SHIFT: u32 = 16;
const SURFACE_GREEN_SHIFT: u32 = 8;

const RECTANGLE_COLOR: u32 = 0x00FF_00FF;

const DRIVER_MEMORY: u64 = 16 * 1024 * 1024;

const DISPLAY_IMPORTS: &[crate::composition::Import] = &[crate::composition::Import {
    declaration: RequirementDeclaration::all(DISPLAY_REQUIREMENT, display::Protocol::ID),
    namespace_root: None,
}];

fn screens(importer: &Importer) -> Vec<Found> {
    importer.found::<display::Protocol>(DISPLAY_REQUIREMENT)
}

fn opened(screen: &Found) -> display::Client<SessionTransport> {
    {
        let executor = crate::executor::executor();
        executor.block_on(component_startup::open::<display::Protocol>(
            screen,
            &executor.reactor(),
            SCREEN_REPLY_TIMEOUT,
        ))
    }
    .expect("screen opens")
    .client()
}

fn fill(
    surface: &Mapping,
    dimension: DisplayDimension,
    area: Area,
    color: impl Fn(u32, u32) -> u32,
) {
    let stride = dimension.surface_stride();
    let mut row = Vec::with_capacity(area.width as usize * SURFACE_PIXEL_BYTES as usize);
    for y in area.y..area.y + area.height {
        row.clear();
        for x in area.x..area.x + area.width {
            row.extend_from_slice(&color(x, y).to_le_bytes());
        }
        let offset = u64::from(y) * stride + u64::from(area.x) * u64::from(SURFACE_PIXEL_BYTES);
        surface
            .copy_in_at(offset as usize, &row)
            .expect("row inside the surface");
    }
}

fn channel(position: u32, side: u32) -> u32 {
    (u64::from(position) * SURFACE_CHANNEL_MAX / u64::from(side)) as u32
}

fn whole(dimension: DisplayDimension) -> Area {
    Area {
        x: 0,
        y: 0,
        width: dimension.width,
        height: dimension.height,
    }
}

#[test_harness::test]
fn each_screen_shows_a_gradient_and_a_rectangle() {
    let composition = with_drivers();
    let importer = composition.importer(DISPLAY_IMPORTS);
    let found = screens(&importer);

    for screen in &found {
        let session = opened(screen);
        let dimension = session.dimension().wait().expect("dimension call");
        let region = MemoryRegion::from(session.acquire_surface().wait().expect("surface granted"));
        let surface = Mapping::full(region, UserMemFlags::ReadWrite).expect("surface maps");
        let rectangle = Area {
            x: dimension.width / 4,
            y: dimension.height / 4,
            width: dimension.width / 2,
            height: dimension.height / 2,
        };

        fill(&surface, dimension, whole(dimension), |x, y| {
            (channel(x, dimension.width) << SURFACE_RED_SHIFT)
                | (channel(y, dimension.height) << SURFACE_GREEN_SHIFT)
        });
        let gradient = session.present(whole(dimension)).wait();
        fill(&surface, dimension, rectangle, |_, _| RECTANGLE_COLOR);
        let shown = session.present(rectangle).wait();

        test_harness::check_eq!(gradient, Ok(()));
        test_harness::check_eq!(shown, Ok(()));
        test_harness::report!(
            "the {}x{} screen shows the gradient and the rectangle",
            dimension.width,
            dimension.height
        );
    }
    finish(composition);

    if found.is_empty() {
        test_harness::skip!("no screen on this machine");
    }
}

#[test_harness::test]
fn surfaces_of_a_screen_end_with_the_budget_and_return_with_their_sessions() {
    let composition = with_drivers();
    let importer = composition.importer(DISPLAY_IMPORTS);
    let found = screens(&importer);

    for screen in &found {
        let mut screens = Vec::new();
        // Каждая поверхность занимает не меньше страницы бюджета драйвера.
        let refusal = (0..=DRIVER_MEMORY / USERLAND_PAGE_SIZE).find_map(|_| {
            let session = opened(screen);
            let before = session
                .present(whole(session.dimension().wait().expect("dimension call")))
                .wait();

            test_harness::check_eq!(before, Err(CallError::Remote(DisplayError::NoSurface)));
            match session.acquire_surface().wait() {
                Ok(region) => {
                    screens.push((session, MemoryRegion::from(region)));
                    None
                }
                Err(error) => Some(error),
            }
        });
        let held = screens.len();
        drop(screens);
        let reopened = opened(screen);
        let regained = reopened.acquire_surface().wait().map(drop);

        test_harness::check_eq!(refusal, Some(CallError::Remote(DisplayError::NoSpace)));
        test_harness::check!(held >= 1);
        test_harness::check_eq!(regained, Ok(()));
        test_harness::report!("the screen held {} surfaces", held);
    }
    finish(composition);

    if found.is_empty() {
        test_harness::skip!("no screen on this machine");
    }
}
