use core::{cell::Cell, time::Duration};
use std::{rc::Rc, thread};

use component_startup::{SessionPort, serve_calls};
use components::component_manifest;
use display_abi::{Area, DisplayDimension, DisplayError, PhysicalSize, display};
use executor::{Executor, Reactor};
use input_abi::{
    AxisList, Binding, DescribeError, DeviceDescriptor, DeviceLayout, DeviceNumber, KeyList,
    NextError, Record, RecordBuf, Usage, input,
};
use ipc::{IntoWireValue, wire::Cap};
use kernel_objects::{Delivery, MemoryRegion, Resource, Session, Signal};
use slint::{ComponentHandle, Timer, TimerMode};
use slint_display::ScreenPlatform;
use syscall::{MemoryAccess, Rights, SIGNALED, Timeout, WaitItem, kind};

use crate::ui::TestWindow;

const DIMENSION: DisplayDimension = DisplayDimension {
    width: 64,
    height: 32,
};
const CHECK_TIMEOUT: Timeout = Timeout::from_secs(3);

component_manifest! { struct StandStartup {} }

struct Screen {
    region: MemoryRegion,
    started: Rc<Signal>,
    delivered: Rc<Signal>,
    presents: Cell<u32>,
    reactor: Reactor,
}

struct Input {
    started: Rc<Signal>,
    sent: Cell<bool>,
    reactor: Reactor,
}

#[allow(clippy::unused_async_trait_impl)]
impl display::Service<Delivery> for Screen {
    fn dimension(&self, _frame: &Delivery) -> DisplayDimension {
        DIMENSION
    }

    fn physical_size(&self, _frame: &Delivery) -> Option<PhysicalSize> {
        None
    }

    fn acquire_surface(
        &self,
        _frame: &Delivery,
    ) -> impl IntoWireValue<Delivery, Read = Result<Cap<kind::MemoryRegion>, DisplayError>> {
        self.region
            .duplicate(Rights::READ | Rights::WRITE | Rights::TRANSFER, None)
            .map_err(|_| DisplayError::NoSpace)
    }

    async fn present(&self, _frame: &Delivery, area: Area) -> Result<(), DisplayError> {
        DIMENSION.check_area(area)?;
        let count = self.presents.get();
        self.presents.set(count + 1);
        if count == 0 {
            self.started.notify_all().expect("present starts");
            self.reactor
                .wait(WaitItem::new(self.delivered.as_raw(), SIGNALED))
                .await
                .expect("input is delivered before present finishes");
        }

        Ok(())
    }
}

#[allow(clippy::unused_async_trait_impl)]
impl input::Service<Delivery> for Input {
    async fn next(&self, _frame: &Delivery) -> Result<RecordBuf, NextError> {
        if self.sent.replace(true) {
            return core::future::pending().await;
        }

        self.reactor
            .wait(WaitItem::new(self.started.as_raw(), SIGNALED))
            .await
            .expect("present is pending");
        let mut records = RecordBuf::new();
        for record in [
            Record::device_added(DeviceNumber::FIRST),
            Record::key(Some(DeviceNumber::FIRST), 0, true),
            Record::frame(Some(DeviceNumber::FIRST), 0),
        ] {
            records.push(record).expect("input frame fits");
        }

        Ok(records)
    }

    #[allow(clippy::unwrap_in_result)]
    fn describe(
        &self,
        _frame: &Delivery,
        _device: DeviceNumber,
    ) -> Result<DeviceLayout, DescribeError> {
        let mut keys = KeyList::new();
        keys.push(Usage::ENTER).expect("one key fits");
        let axes = AxisList::new();
        Ok(
            DeviceDescriptor::new("test key", Binding::None, &keys, &axes, 0, &axes)
                .expect("descriptor fits")
                .layout()
                .expect("valid layout"),
        )
    }
}

pub(crate) fn main() {
    let _ = components::requirements::<StandStartup>().expect("startup is present");
    let executor = Rc::new(Executor::new().expect("executor"));
    let reactor = executor.reactor();
    executor.block_on(async {
        reactor
            .timeout(CHECK_TIMEOUT, check(&executor))
            .await
            .expect("Slint completes its checks");
    });
    println!("slint checks passed");
}

async fn check(executor: &Rc<Executor>) {
    let reactor = executor.reactor();
    let resource = Resource::self_resource().expect("process resource");
    let started = Rc::new(Signal::create().expect("present signal"));
    let delivered = Rc::new(Signal::create().expect("input signal"));
    let screen = Rc::new(Screen {
        region: resource
            .create_virtual(
                DIMENSION.surface_size().expect("surface size"),
                MemoryAccess::RW,
            )
            .expect("surface"),
        started: started.clone(),
        delivered: delivered.clone(),
        presents: Cell::new(0),
        reactor: reactor.clone(),
    });

    let screens = Rc::new(SessionPort::create().expect("display port"));
    let display = Session::from_handle(screens.create_session().expect("display session").port);

    let serving = screen.clone();
    let serving = serve_calls(
        executor,
        screens,
        move |frame| display::serve(&serving, frame),
        |_| {},
    );
    let _screen = executor.spawn_now(serving).abort_on_drop();

    let inputs = Rc::new(SessionPort::create().expect("input port"));
    let input = Session::from_handle(inputs.create_session().expect("input session").port);

    let source = Rc::new(Input {
        started,
        sent: Cell::new(false),
        reactor: reactor.clone(),
    });

    let serving = serve_calls(
        executor,
        inputs,
        move |frame| input::serve(&source, frame),
        |_| {},
    );
    let _input = executor.spawn_now(serving).abort_on_drop();

    let mut platform = ScreenPlatform::open(display, reactor)
        .await
        .expect("screen opens");

    let view = TestWindow::new().expect("UI");
    let pressed = Rc::new(Cell::new(false));
    let received = pressed.clone();
    let main_thread = thread::current().id();
    view.on_pressed(move || {
        assert_eq!(thread::current().id(), main_thread);
        received.set(true);
        delivered.notify_all().expect("input completes present");
    });

    view.show().expect("window shows");

    platform.deliver_input(executor, input, &view);
    platform
        .run_until(|| pressed.get().then_some(()))
        .await
        .expect("input arrives during present");
    assert!(screen.presents.get() > 0);

    check_queue(&mut platform, &view).await;
}

async fn check_queue(platform: &mut ScreenPlatform, view: &TestWindow) {
    let main_thread = thread::current().id();
    let weak = view.as_weak();
    let worker = thread::spawn(move || {
        thread::sleep(Duration::from_millis(20));
        weak.upgrade_in_event_loop(move |view| {
            assert_eq!(thread::current().id(), main_thread);
            view.set_posted(true);
        })
        .expect("worker posts to UI");
    });
    let mut turns = 0;
    platform
        .run_until(|| {
            turns += 1;
            view.get_posted().then_some(())
        })
        .await
        .expect("posted task wakes idle UI");
    worker.join().expect("worker exits");
    assert!(turns <= 2, "idle UI spins between events");

    view.set_posted(false);
    let weak = view.as_weak();
    let timer = Timer::default();
    timer.start(
        TimerMode::SingleShot,
        Duration::from_millis(20),
        move || {
            weak.upgrade_in_event_loop(|view| view.set_posted(true))
                .expect("timer posts");
        },
    );
    platform
        .run_until(|| view.get_posted().then_some(()))
        .await
        .expect("Slint timer wakes UI");
}
