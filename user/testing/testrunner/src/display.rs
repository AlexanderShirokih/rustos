//! Контракт `Display` на сервере `display-fixture`.

use alloc::vec::Vec;

use component_abi::{Attribute, RequirementDeclaration};
use component_startup::{Attributes, Found};
use display_abi::{Area, DisplayError, display};
use ipc::{CallError, Protocol};
use kernel_objects::{MemoryRegion, SessionTransport};
use runtime::Timeout;
use userland_image::ComponentSpec;

use crate::{
    composition::Importer,
    display_fixture_protocol::DIMENSION,
    log_relay::await_component_lines,
    rootkeeper_system::{await_revoked, finish, fixture, indexed, query, registered, scenario},
};

const SCREEN: &str = "screen";

const DISPLAY_REPLY_TIMEOUT: Timeout = Timeout::from_secs(2);

const WHOLE_SCREEN: Area = Area {
    x: 0,
    y: 0,
    width: DIMENSION.width,
    height: DIMENSION.height,
};

const FIXTURE_QUERY: &[Attribute<'static>] = &[Attribute {
    key: b"kind",
    value: b"fixture",
}];

const DISPLAY: ComponentSpec<'static> = ComponentSpec {
    ..fixture("/display-fixture", "display-fixture")
};

const SCREEN_REQUIREMENTS: &[crate::composition::Import] = &[crate::composition::Import {
    declaration: RequirementDeclaration::session(SCREEN, display::Protocol::ID),

    namespace_root: None,
}];

const SCREENS: &[crate::composition::Import] = &[crate::composition::Import {
    declaration: RequirementDeclaration::all(SCREEN, display::Protocol::ID),

    namespace_root: None,
}];

fn session(importer: &Importer) -> display::Client<SessionTransport> {
    let bytes = query(FIXTURE_QUERY);
    component_startup::session::<display::Protocol>(
        importer.handle(),
        SCREEN,
        Attributes::decode(&bytes).expect("query decodes"),
        DISPLAY_REPLY_TIMEOUT,
    )
    .expect("session granted")
    .client()
    .with_wait(DISPLAY_REPLY_TIMEOUT)
}

fn found(importer: &Importer) -> Vec<Found> {
    let bytes = query(FIXTURE_QUERY);
    importer
        .found_with::<display::Protocol>(SCREEN, Attributes::decode(&bytes).expect("query decodes"))
}

fn surface(
    session: &display::Client<SessionTransport>,
) -> Result<MemoryRegion, CallError<DisplayError>> {
    session.acquire_surface().map(MemoryRegion::from)
}

fn present(
    session: &display::Client<SessionTransport>,
    area: Area,
) -> Result<(), CallError<DisplayError>> {
    session.present(area)
}

#[test_harness::test]
fn a_display_session_gets_the_dimension_and_a_surface_that_holds_it() {
    let composition = scenario(&[DISPLAY]);
    let importer = composition.importer(SCREEN_REQUIREMENTS);
    let screen = session(&importer);

    let dimension = screen.dimension().expect("dimension call");
    let region = surface(&screen).expect("surface granted");

    test_harness::check_eq!(dimension, DIMENSION);
    let size = region.inspect().expect("surface inspects").size_bytes;
    test_harness::check!(
        dimension
            .surface_size()
            .is_some_and(|needed| size >= needed)
    );
    drop((screen, region));
    finish(composition);
}

#[test_harness::test]
fn a_second_surface_request_of_a_session_keeps_the_first_surface() {
    let composition = scenario(&[DISPLAY]);
    let importer = composition.importer(SCREEN_REQUIREMENTS);
    let screen = session(&importer);
    let region = surface(&screen).expect("surface granted");

    let again = surface(&screen).map(drop);

    test_harness::check_eq!(again, Err(CallError::Remote(DisplayError::AlreadyAcquired)));
    test_harness::check!(region.inspect().is_ok());
    drop((screen, region));
    finish(composition);
}

#[test_harness::test]
fn a_present_before_the_surface_is_no_surface() {
    let composition = scenario(&[DISPLAY]);
    let importer = composition.importer(SCREEN_REQUIREMENTS);
    let screen = session(&importer);

    test_harness::check_eq!(
        present(&screen, WHOLE_SCREEN),
        Err(CallError::Remote(DisplayError::NoSurface))
    );
    drop(screen);
    finish(composition);
}

#[test_harness::test]
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

    test_harness::check_eq!(
        present(&screen, past_edge),
        Err(CallError::Remote(DisplayError::OutOfBounds))
    );
    test_harness::check_eq!(
        present(&screen, empty),
        Err(CallError::Remote(DisplayError::OutOfBounds))
    );
    drop((screen, region));
    finish(composition);
}

#[test_harness::test]
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

    test_harness::check_eq!(present(&screen, corner), Ok(()));
    drop((screen, region));
    finish(composition);
}

#[test_harness::test]
fn a_surface_over_the_budget_is_granted_after_the_holder_session_closes() {
    let composition = scenario(&[DISPLAY]);
    let importer = composition.importer(SCREEN_REQUIREMENTS);
    let first = session(&importer);
    let second = session(&importer);
    let held = surface(&first).expect("first session takes the budget");

    test_harness::check_eq!(
        surface(&second).map(drop),
        Err(CallError::Remote(DisplayError::NoSpace))
    );

    drop(first);

    test_harness::check!(await_revoked(|| held.inspect()));
    drop(held);
    test_harness::check!(surface(&second).is_ok());
    drop(second);
    finish(composition);
}

#[test_harness::test]
fn a_session_starts_an_image_program_under_its_index_name() {
    let (composition, _) = indexed(&[]);
    let importer = composition.importer(SCREENS);
    let screen = session(&importer);

    let dimension = screen.dimension().expect("dimension call");
    let lines = await_component_lines(DISPLAY.name, 1);

    test_harness::check_eq!(dimension, DIMENSION);
    test_harness::check_eq!(lines, ["started"]);
    drop(screen);
    finish(composition);
}

#[test_harness::test]
fn an_enumeration_finds_an_image_program_without_starting_it() {
    let (composition, names) = indexed(&[]);
    let importer = composition.importer(SCREENS);

    let found = found(&importer).len();
    let record = registered(&composition, &names, DISPLAY);

    test_harness::check_eq!(found, 1);
    test_harness::check!(matches!(record, Ok(1)));
    finish(composition);
}

#[test_harness::test]
fn an_open_after_the_session_reaches_the_started_image_program() {
    let (composition, names) = indexed(&[]);
    let importer = composition.importer(SCREENS);
    let found = found(&importer);
    let screen = session(&importer);

    let opened = found.first().map(|found| {
        component_startup::open::<display::Protocol>(found, DISPLAY_REPLY_TIMEOUT).map(drop)
    });
    let record = registered(
        &composition,
        &names,
        ComponentSpec {
            name: "second-display",
            ..DISPLAY
        },
    );

    test_harness::check_eq!(opened, Some(Ok(())));
    test_harness::check!(matches!(record, Ok(2)));
    drop(screen);
    finish(composition);
}

#[test_harness::test]
fn an_image_program_with_a_plan_record_is_found_once() {
    let (composition, _) = indexed(&[DISPLAY]);
    let importer = composition.importer(SCREENS);

    let found = found(&importer).len();
    let dimension = session(&importer).dimension().expect("dimension call");

    test_harness::check_eq!(found, 1);
    test_harness::check_eq!(dimension, DIMENSION);
    finish(composition);
}
