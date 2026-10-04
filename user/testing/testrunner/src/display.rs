//! Контракт `Display` на сервере `display-fixture`.

use component_abi::RequirementDeclaration;
use display_abi::{Area, DisplayError, display};
use ipc::Protocol;
use kernel_objects::{MemoryRegion, SessionTransport};
use kernel_tests::kernel_test;
use runtime::Timeout;
use userland_image::ComponentSpec;

use crate::{
    composition::Importer,
    display_fixture_protocol::DIMENSION,
    rootkeeper_system::{await_revoked, finish, fixture, scenario},
};

const SCREEN: &str = "screen";

const DISPLAY_REPLY_TIMEOUT: Timeout = Timeout::from_secs(2);

const WHOLE_SCREEN: Area = Area {
    x: 0,
    y: 0,
    width: DIMENSION.width,
    height: DIMENSION.height,
};

const DISPLAY: ComponentSpec<'static> = ComponentSpec {
    ..fixture("/display-fixture", "display-fixture")
};

const SCREEN_REQUIREMENTS: &[crate::composition::Import] = &[crate::composition::Import {
    declaration: RequirementDeclaration::session(SCREEN, display::Protocol::ID),

    namespace_root: None,
}];

fn session(importer: &Importer) -> display::Client<SessionTransport> {
    importer
        .session::<display::Protocol>(SCREEN)
        .client()
        .with_wait(DISPLAY_REPLY_TIMEOUT)
}

fn surface(session: &display::Client<SessionTransport>) -> Result<MemoryRegion, DisplayError> {
    session
        .acquire_surface()
        .expect("acquire_surface call")
        .map(MemoryRegion::from)
}

fn present(session: &display::Client<SessionTransport>, area: Area) -> Result<(), DisplayError> {
    session.present(area).expect("present call")
}

#[kernel_test]
fn a_display_session_gets_the_dimension_and_a_surface_that_holds_it() {
    let composition = scenario(&[DISPLAY]);
    let importer = composition.importer(SCREEN_REQUIREMENTS);
    let screen = session(&importer);

    let dimension = screen.dimension().expect("dimension call");
    let region = surface(&screen).expect("surface granted");

    kernel_tests::kassert_eq!(dimension, DIMENSION);
    let size = region.inspect().expect("surface inspects").size_bytes;
    kernel_tests::kassert!(
        dimension
            .surface_size()
            .is_some_and(|needed| size >= needed)
    );
    drop((screen, region));
    finish(composition);
}

#[kernel_test]
fn a_second_surface_request_of_a_session_keeps_the_first_surface() {
    let composition = scenario(&[DISPLAY]);
    let importer = composition.importer(SCREEN_REQUIREMENTS);
    let screen = session(&importer);
    let region = surface(&screen).expect("surface granted");

    let again = surface(&screen).map(drop);

    kernel_tests::kassert_eq!(again, Err(DisplayError::AlreadyAcquired));
    kernel_tests::kassert!(region.inspect().is_ok());
    drop((screen, region));
    finish(composition);
}

#[kernel_test]
fn a_present_before_the_surface_is_no_surface() {
    let composition = scenario(&[DISPLAY]);
    let importer = composition.importer(SCREEN_REQUIREMENTS);
    let screen = session(&importer);

    kernel_tests::kassert_eq!(present(&screen, WHOLE_SCREEN), Err(DisplayError::NoSurface));
    drop(screen);
    finish(composition);
}

#[kernel_test]
fn a_present_past_the_edge_or_of_an_empty_area_is_out_of_bounds() {
    let composition = scenario(&[DISPLAY]);
    let importer = composition.importer(SCREEN_REQUIREMENTS);
    let screen = session(&importer);
    let region = surface(&screen).expect("surface granted");

    let past_edge = Area {
        x: DIMENSION.width - 4,
        width: 8,
        ..WHOLE_SCREEN
    };
    let empty = Area {
        height: 0,
        ..WHOLE_SCREEN
    };

    kernel_tests::kassert_eq!(present(&screen, past_edge), Err(DisplayError::OutOfBounds));
    kernel_tests::kassert_eq!(present(&screen, empty), Err(DisplayError::OutOfBounds));
    drop((screen, region));
    finish(composition);
}

#[kernel_test]
fn a_present_inside_the_screen_succeeds() {
    let composition = scenario(&[DISPLAY]);
    let importer = composition.importer(SCREEN_REQUIREMENTS);
    let screen = session(&importer);
    let region = surface(&screen).expect("surface granted");

    let corner = Area {
        x: DIMENSION.width - 8,
        y: DIMENSION.height - 4,
        width: 8,
        height: 4,
    };

    kernel_tests::kassert_eq!(present(&screen, corner), Ok(()));
    drop((screen, region));
    finish(composition);
}

#[kernel_test]
fn a_surface_over_the_budget_is_granted_after_the_holder_session_closes() {
    let composition = scenario(&[DISPLAY]);
    let importer = composition.importer(SCREEN_REQUIREMENTS);
    let first = session(&importer);
    let second = session(&importer);
    let held = surface(&first).expect("first session takes the budget");

    kernel_tests::kassert_eq!(surface(&second).map(drop), Err(DisplayError::NoSpace));

    drop(first);

    kernel_tests::kassert!(await_revoked(|| held.inspect()));
    drop(held);
    kernel_tests::kassert!(surface(&second).is_ok());
    drop(second);
    finish(composition);
}
