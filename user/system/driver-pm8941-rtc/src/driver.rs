use alloc::{
    rc::{Rc, Weak},
    vec::Vec,
};
use core::{
    cell::RefCell,
    fmt,
    task::{Poll, Waker},
};

use component_startup::{
    AnyOf, AsyncStartup, Providers, Requirements as _, SessionError, SessionPort, StartupError,
    component_manifest, serve_component,
};
use executor::{Executor, Mutex, PortChannel};
use ipc::CallError;
use kernel_objects::{Port, Session, SessionTransport, Signal};
use platform_abi::{CONTROLLER_KEY, DescriptionError, MappedDescription, PlatformError, platform};
use rtc_abi::rtc;
use runtime::{FAILED_EXIT_CODE, Timeout, exit, log_fmt};
use spmi_abi::{DeviceError, SpmiError, spmi, spmi_device, spmi_interrupt};
use syscall::{Handle, SIGNALED, WaitItem, kind};

type Channel = PortChannel<SessionTransport>;

const COMPATIBLE: &str = "qcom,pm8941-rtc";

const RTC_CONTROL: u16 = 0x6046;
const COUNT: u16 = 0x6048;
const ALARM_VALUE: u16 = 0x6140;
const ALARM_CONTROL: u16 = 0x6146;
const ALARM_CONTROL_2: u16 = 0x6148;

const RTC_ENABLE: u8 = 1 << 7;
const ALARM_ENABLE: u8 = 1 << 7;
const ALARM_CLEAR: u8 = 1 << 0;

const COUNT_BYTES: u8 = 4;
const LOW_BYTE: u64 = 0xFF;

const ALARM_INTERRUPT: u16 = 0;
const ALARM_SECONDS: u64 = 2;
const ALARM_BUDGET: Timeout = Timeout::from_secs(10);

const BUS_CALL_TIMEOUT: Timeout = Timeout::from_secs(2);

component_manifest! {
    struct RtcStartup {
        requirements: {
            platform: session(platform::Protocol),
            spmi: all(spmi::Protocol),
        },
        features: ["qcom,pm8941-rtc"],
        exports: [rtc::Protocol],
    }
}

struct Shared {
    sessions: Rc<SessionPort>,
    clock: RefCell<Option<Rc<Clock>>>,
}

struct Clock {
    _bus: spmi::Client<Channel>,
    device: Rc<spmi_device::Client<Channel>>,
    alarm: Mutex<Option<Rc<Alarm>>>,
    executor: Weak<Executor>,
}

struct Alarm {
    interrupt: spmi_interrupt::Client<Channel>,
    signal: Signal,
    device: Rc<spmi_device::Client<Channel>>,
    state: RefCell<AlarmState>,
}

struct AlarmState {
    generation: u64,
    requested: bool,
    worker: Option<Waker>,
    waiting: Vec<Waker>,
}

#[derive(Debug)]
enum Stopped {
    Startup(StartupError),
    ServicePort,
    Executor,
    ServicePortClosed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Failure {
    Platform(CallError<PlatformError>),
    Mapping(kernel_objects::Error),
    Description(DescriptionError),
    NoNode,
    NoController,
    NoBus,
    Startup(StartupError),
    Bus(CallError<SpmiError>),
    Device(CallError<DeviceError>),
    Silent,
}

impl fmt::Display for Stopped {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Startup(error) => error.fmt(formatter),
            Self::ServicePort => formatter.write_str("service port refused"),
            Self::Executor => formatter.write_str("executor refused"),
            Self::ServicePortClosed => formatter.write_str("service port closed"),
        }
    }
}

impl Shared {
    async fn open(
        &self,
        platform: &platform::Client<Channel>,
        spmi: &Providers<spmi::Protocol>,
        executor: &Weak<Executor>,
    ) -> Result<Port, SessionError> {
        if self.clock.borrow().is_none() {
            let clock = Clock::bind(platform, spmi, executor.clone())
                .await
                .map_err(|failure| {
                    log_fmt(format_args!("{COMPATIBLE} not bound: {failure:?}"));
                    SessionError::Refused
                })?;
            *self.clock.borrow_mut() = Some(Rc::new(clock));
            log_fmt(format_args!("{COMPATIBLE} bound"));
        }

        self.sessions
            .create_session()
            .map(|minted| minted.port)
            .map_err(|_| SessionError::NoSpace)
    }
}

impl Clock {
    async fn bind(
        platform: &platform::Client<Channel>,
        spmi: &Providers<spmi::Protocol>,
        executor: Weak<Executor>,
    ) -> Result<Self, Failure> {
        let description = platform.description().await.map_err(Failure::Platform)?;
        let mapped = MappedDescription::new(description)
            .map_err(|refused| Failure::Mapping(refused.error))?;
        let description = mapped.description().map_err(Failure::Description)?;

        let id = description
            .compatible_devices(COMPATIBLE)
            .next()
            .map(|device| device.id().clone())
            .ok_or(Failure::NoNode)?;
        let controller = spmi_abi::controller(&description, &id).ok_or(Failure::NoController)?;
        let compatible = controller
            .strings("compatible")
            .map_err(Failure::Description)?
            .ok_or(Failure::NoController)?;

        let session = spmi
            .open_first(AnyOf::new(CONTROLLER_KEY, compatible))
            .await
            .map_err(Failure::Startup)?
            .ok_or(Failure::NoBus)?;

        let bus = PortChannel::client(session).with_wait(BUS_CALL_TIMEOUT);

        let device = bus.open(id.clone()).await.map_err(Failure::Bus)?;

        let clock = Self {
            _bus: bus,
            device: Rc::new(
                PortChannel::client(Session::<spmi_device::Protocol>::from_session(device))
                    .with_wait(BUS_CALL_TIMEOUT),
            ),
            alarm: Mutex::new(None),
            executor,
        };

        clock
            .device
            .update(RTC_CONTROL, RTC_ENABLE, RTC_ENABLE)
            .await?;

        Ok(clock)
    }

    async fn alarm(&self) -> Result<u64, Failure> {
        let mut slot = self.alarm.lock().await;
        if slot.is_none() {
            let interrupt = PortChannel::client(Session::<spmi_interrupt::Protocol>::from_session(
                self.device.irq(ALARM_INTERRUPT).await?,
            ))
            .with_wait(BUS_CALL_TIMEOUT);
            let signal = Signal::from(interrupt.signal().await?);

            let alarm = Rc::new(Alarm {
                interrupt,
                signal,
                device: self.device.clone(),
                state: RefCell::new(AlarmState {
                    generation: 0,
                    requested: false,
                    worker: None,
                    waiting: Vec::new(),
                }),
            });

            self.executor
                .upgrade()
                .ok_or(Failure::Silent)?
                .spawn(alarm.clone().watch());
            *slot = Some(alarm);
        }

        let alarm = slot.as_ref().ok_or(Failure::Silent)?;
        let pending = {
            let state = alarm.state.borrow();
            state.requested.then_some(state.generation)
        };

        if let Some(generation) = pending {
            alarm.completed(generation).await?;
        }

        let (generation, worker) = {
            let mut state = alarm.state.borrow_mut();
            state.requested = true;
            (state.generation, state.worker.take())
        };

        if let Some(worker) = worker {
            worker.wake();
        }

        alarm.completed(generation).await?;
        count(&self.device).await
    }
}

impl Alarm {
    async fn completed(&self, generation: u64) -> Result<(), Failure> {
        let waiting = core::future::poll_fn(|cx| {
            let mut state = self.state.borrow_mut();
            if state.generation != generation {
                return Poll::Ready(());
            }

            if !state
                .waiting
                .iter()
                .any(|waker| waker.will_wake(cx.waker()))
            {
                state.waiting.push(cx.waker().clone());
            }

            Poll::Pending
        });

        executor::timeout(ALARM_BUDGET, waiting)
            .await
            .ok_or(Failure::Silent)
    }

    async fn watch(self: Rc<Self>) {
        loop {
            if let Err(failure) = self.cycle().await {
                stop("alarm", failure);
            }
        }
    }

    async fn cycle(&self) -> Result<(), Failure> {
        core::future::poll_fn(|cx| {
            let mut state = self.state.borrow_mut();
            if state.requested {
                Poll::Ready(())
            } else {
                state.worker = Some(cx.waker().clone());
                Poll::Pending
            }
        })
        .await;

        let target = (count(&self.device).await? + ALARM_SECONDS) & u64::from(u32::MAX);

        self.device.update(ALARM_CONTROL, ALARM_ENABLE, 0).await?;
        self.device.write(ALARM_VALUE, COUNT_BYTES, target).await?;
        self.device
            .update(ALARM_CONTROL, ALARM_ENABLE, ALARM_ENABLE)
            .await?;

        executor::timeout(
            ALARM_BUDGET,
            executor::wait(WaitItem::new(self.signal.as_raw(), SIGNALED)),
        )
        .await
        .ok_or(Failure::Silent)?
        .map_err(|_| Failure::Silent)?;

        self.device.update(ALARM_CONTROL, ALARM_ENABLE, 0).await?;
        self.device.update(ALARM_CONTROL_2, ALARM_CLEAR, 0).await?;
        self.interrupt.ack().await?;

        let waiting = {
            let mut state = self.state.borrow_mut();
            state.generation = state.generation.wrapping_add(1);
            state.requested = false;
            core::mem::take(&mut state.waiting)
        };

        for waker in waiting {
            waker.wake();
        }

        Ok(())
    }
}

impl<F: ipc::ReceivedResources> rtc::Service<F> for Clock {
    async fn now(&self, _frame: &F) -> u64 {
        count(&self.device)
            .await
            .unwrap_or_else(|failure| stop("count", failure))
    }

    async fn tick(&self, _frame: &F) -> u64 {
        self.alarm()
            .await
            .unwrap_or_else(|failure| stop("alarm", failure))
    }
}

impl From<CallError<DeviceError>> for Failure {
    fn from(error: CallError<DeviceError>) -> Self {
        Self::Device(error)
    }
}

fn stop(operation: &str, failure: Failure) -> ! {
    log_fmt(format_args!("{operation} failed: {failure:?}"));
    exit(FAILED_EXIT_CODE)
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Stopped> {
    let requirements = RtcStartup::from_parent(parent);

    let executor = Rc::new(Executor::new().map_err(|_| Stopped::Executor)?);

    let platform = PortChannel::client(requirements.platform.open().map_err(Stopped::Startup)?)
        .with_wait(BUS_CALL_TIMEOUT);

    let shared = Rc::new(Shared {
        sessions: Rc::new(SessionPort::create().map_err(|_| Stopped::ServicePort)?),
        clock: RefCell::new(None),
    });

    let open = shared.clone();
    let spawning = Rc::downgrade(&executor);

    let served = serve_component::<rtc::Protocol, _>(
        &executor,
        AsyncStartup::new(PortChannel::new(parent)),
        shared.sessions.clone(),
        move |frame| {
            let clock = shared.clock.borrow().clone();
            let clock = clock.ok_or(ipc::wire::IpcError::PeerClosed)?;
            rtc::serve(&clock, frame)
        },
        async move |_| open.open(&platform, &requirements.spmi, &spawning).await,
        |_| {},
    );

    let _ = executor.block_on(served);
    Err(Stopped::ServicePortClosed)
}

async fn count(device: &spmi_device::Client<Channel>) -> Result<u64, Failure> {
    let count = device.read(COUNT, COUNT_BYTES).await?;
    let low = device.read(COUNT, 1).await?;
    if carried(count, low) {
        return Ok(device.read(COUNT, COUNT_BYTES).await?);
    }

    Ok(count)
}

fn carried(count: u64, low: u64) -> bool {
    // Младший байт, ставший меньше, перенёсся в старшие между чтениями.
    low < (count & LOW_BYTE)
}
