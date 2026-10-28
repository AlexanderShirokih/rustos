use alloc::{boxed::Box, sync::Arc};
use core::task::Poll;

use executor::PortChannel;
use ipc::{
    CallError, Listener as _,
    wire::{IpcError, MESSAGE_INLINE_MAX},
};
use kernel_objects::{Delivery, Port, Session, Signal};
use runtime::{JoinHandle, Mutex, PortTransport, Timeout, spawn_owned};
use syscall::{Rights, SIGNALED, WaitItem};

use crate::{
    async_component_protocol::{async_probe, slow_bus},
    composition::pause,
    parent::{Answer, Child, Parent},
};

const FIXTURE: &str = "/async-component-fixture/exec";
const REQUEST_TIMEOUT: Timeout = Timeout::from_secs(5);
const BUS_VALUE: u64 = 71;

type Probe = Session<async_probe::Protocol>;
type SlowAnswer = Result<u64, CallError<u8>>;

struct Bus {
    entered: Arc<Signal>,
    release: Arc<Signal>,
}

struct PendingBus {
    port: Port,
    entered: Arc<Signal>,
    release: Arc<Signal>,
    thread: JoinHandle,
}

impl slow_bus::Service<Delivery> for Bus {
    async fn wait(&self, _frame: &Delivery) -> u64 {
        self.entered.notify_all().expect("bus request entered");
        executor::timeout(
            REQUEST_TIMEOUT,
            executor::wait(WaitItem::new(self.release.as_raw(), SIGNALED)),
        )
        .await
        .expect("bus release before timeout")
        .expect("bus released");
        BUS_VALUE
    }
}

impl PendingBus {
    fn start() -> Self {
        let port = Port::create().expect("bus port");
        let client = port
            .duplicate(Rights::DUPLICATE | Rights::WRITE | Rights::TRANSFER, None)
            .expect("bus client");
        let entered = Arc::new(Signal::create().expect("entered signal"));
        let release = Arc::new(Signal::create().expect("release signal"));
        let observed = entered.clone();
        let resumed = release.clone();

        let thread = spawn_owned(move || {
            let executor = crate::executor::executor();
            let bus = Bus {
                entered: observed,
                release: resumed,
            };
            let mut bytes = [0; MESSAGE_INLINE_MAX];
            let frame = PortTransport::new(port.as_raw())
                .recv(&mut bytes, REQUEST_TIMEOUT)
                .expect("bus frame");
            let frame = ipc::OwnedIncoming::new(frame).expect("bus frame fits");
            let result = executor.block_on(slow_bus::dispatch(&bus, frame));
            assert!(matches!(result, Ok(()) | Err(IpcError::PeerClosed)));
            0
        })
        .expect("bus thread");
        Self {
            port: client,
            entered,
            release,
            thread,
        }
    }

    fn session(&self) -> Session<slow_bus::Protocol> {
        Session::from_handle(
            self.port
                .duplicate(Rights::WRITE | Rights::TRANSFER, None)
                .expect("bus session"),
        )
    }

    fn finish(self) {
        self.release.notify_all().expect("release bus");
        self.thread.join(REQUEST_TIMEOUT).expect("bus thread joins");
    }
}

fn open(parent: &Parent, child: u16) -> (Port, Probe) {
    let importer = parent.importer(alloc::vec![(
        "probe",
        Answer::export::<async_probe::Protocol>(child)
    )]);
    let session = component_startup::session::<async_probe::Protocol>(
        importer.as_raw(),
        "probe",
        component_startup::Attributes::empty(),
        REQUEST_TIMEOUT,
    )
    .expect("open probe");

    (importer, session)
}

fn slow(probe: &Probe, bus: &PendingBus) -> (JoinHandle, Arc<Mutex<Option<SlowAnswer>>>) {
    let port = probe.handle().as_raw();
    let bus_session = bus.session();
    let answer = Arc::new(Mutex::new(None));
    let stored = answer.clone();

    let thread = spawn_owned(move || {
        *stored.lock() = Some(
            async_probe::Client::new(PortTransport::new(port))
                .with_wait(REQUEST_TIMEOUT)
                .slow(bus_session)
                .wait(),
        );
        0
    })
    .expect("slow caller");

    bus.entered
        .wait(SIGNALED, REQUEST_TIMEOUT)
        .expect("slow call reached bus");

    (thread, answer)
}

#[test_harness::test]
fn async_component_serves_calls_and_exports_while_a_method_waits() {
    let parent = Parent::start();
    let child =
        parent.add(Child::new(FIXTURE, "async-component").export::<async_probe::Protocol>());
    let (_first_importer, first) = open(&parent, child);
    let bus = PendingBus::start();
    let (thread, answer) = slow(&first, &bus);

    test_harness::check_eq!(
        async_probe::Client::new(PortTransport::new(first.handle().as_raw()))
            .quick()
            .wait(),
        Ok(42)
    );
    test_harness::check_eq!(
        async_probe::Client::new(PortTransport::new(first.handle().as_raw()))
            .active()
            .wait(),
        Ok(1)
    );

    let (_second_importer, second) = open(&parent, child);
    test_harness::check_eq!(
        async_probe::Client::new(PortTransport::new(second.handle().as_raw()))
            .quick()
            .wait(),
        Ok(42)
    );
    test_harness::check!(answer.lock().is_none());

    bus.finish();
    thread.join(REQUEST_TIMEOUT).expect("slow caller joins");
    test_harness::check_eq!(answer.lock().take(), Some(Ok(BUS_VALUE)));
    test_harness::check_eq!(
        async_probe::Client::new(PortTransport::new(second.handle().as_raw()))
            .active()
            .wait(),
        Ok(0)
    );

    let bus = PendingBus::start();
    let executor = crate::executor::executor();
    let caller = async_probe::Client::new(PortChannel::new(first.handle().as_raw()))
        .with_wait(REQUEST_TIMEOUT);
    let mut call = Box::pin(core::future::IntoFuture::into_future(
        caller.slow(bus.session()),
    ));

    // Ожидание исхода регистрируется в executor, который опрашивает вызов.
    let started = executor.block_on(core::future::poll_fn(|cx| {
        Poll::Ready(call.as_mut().poll(cx).is_pending())
    }));
    test_harness::check!(started);
    bus.entered
        .wait(SIGNALED, REQUEST_TIMEOUT)
        .expect("cancelled call reached bus");

    drop(call);
    drop(first);

    let deadline = runtime::Instant::now();
    while async_probe::Client::new(PortTransport::new(second.handle().as_raw()))
        .active()
        .wait()
        == Ok(1)
        && runtime::Instant::now().since(deadline) < REQUEST_TIMEOUT
    {
        pause(Timeout::from_millis(1));
    }

    test_harness::check_eq!(
        async_probe::Client::new(PortTransport::new(second.handle().as_raw()))
            .active()
            .wait(),
        Ok(0)
    );

    bus.finish();
    parent.terminate(child, 0);
}
