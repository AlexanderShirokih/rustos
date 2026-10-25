//! Контракт `Application` на стороне `application-window`.

use application_abi::application;
use component_abi::RequirementDeclaration;
use display_abi::display;
use input_abi::input;
use ipc::{CallError, Protocol, wire::IpcError};
use kernel_objects::{Port, Session, SessionTransport};
use runtime::{JoinHandle, Timeout, spawn_owned};
use syscall::Rights;
use userland_image::ComponentSpec;

use crate::{
    composition::{Import, Importer, pause},
    rootkeeper_system::{finish, fixture, scenario},
};

const APPLICATION: &str = "application";

const APPLICATION_FIXTURE: ComponentSpec<'static> =
    fixture("/application-fixture", "application-fixture");

const APPLICATION_IMPORT: &[Import] = &[Import {
    declaration: RequirementDeclaration::session(APPLICATION, application::Protocol::ID),

    namespace_root: None,
}];

const DEPARTURE_TIMEOUT: Timeout = Timeout::from_secs(5);

const RECEIVE_PAUSE: Timeout = Timeout::from_millis(200);

const PEER_CLOSED: u32 = 1;

fn client(session: &Session<application::Protocol>) -> application::Client<SessionTransport> {
    let derived = session
        .handle()
        .duplicate(Rights::WRITE, None)
        .expect("derived application session");
    Session::<application::Protocol>::from_handle(derived)
        .client()
        .with_wait(DEPARTURE_TIMEOUT)
}

fn shown(importer: &Importer) -> Session<application::Protocol> {
    let session = importer.session::<application::Protocol>(APPLICATION);
    let window = Session::<display::Protocol>::from_handle(Port::create().expect("window port"));
    let input = Session::<input::Protocol>::from_handle(Port::create().expect("input port"));
    client(&session)
        .show(window, input)
        .wait()
        .expect("show is answered");

    session
}

fn wait_closed(session: &Session<application::Protocol>) -> JoinHandle {
    let application = client(session);
    spawn_owned(move || {
        u32::from(application.closed().wait() == Err(CallError::Ipc(IpcError::PeerClosed)))
    })
    .expect("closed waiter spawns")
}

#[test_harness::test]
fn closed_answers_peer_closed_when_the_application_exits() {
    let composition = scenario(&[APPLICATION_FIXTURE]);
    let importer = composition.importer(APPLICATION_IMPORT);
    let session = shown(&importer);
    let watcher = wait_closed(&session);
    pause(RECEIVE_PAUSE);

    composition.terminate(APPLICATION_FIXTURE.name, 0);
    let finished = watcher.wait(DEPARTURE_TIMEOUT);
    let closed = watcher.exit_code();

    test_harness::check_eq!(finished, Ok(()));
    test_harness::check_eq!(closed, Ok(PEER_CLOSED));
    let _ = watcher.join(DEPARTURE_TIMEOUT);
    drop(session);
    finish(composition);
}
