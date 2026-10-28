use alloc::rc::{Rc, Weak};
use core::{cell::RefCell, fmt};

use component_startup::{
    AsyncStartup, Requirements as _, SessionError, SessionPort, StartupError, component_manifest,
    serve_component,
};
use executor::{Executor, PortChannel};
use kernel_objects::{IrqLine, SessionTransport};
use pinctrl_msm::{SDM660, Tlmm};
use pins_abi::{Claims, Lines, pins};
use platform_abi::{Description, MappedDescription, platform};
use runtime::{FAILED_EXIT_CODE, exit, log_fmt, log_line};
use syscall::{Handle, SIGNALED, WaitItem, kind};

use crate::{hardware::Hardware, sessions::Controller, windows::Windows};

const COMPATIBLE: &str = "qcom,sdm660-pinctrl";

component_manifest! {
    struct PinsStartup {
        requirements: { platform: session(platform::Protocol) },
        features: ["qcom,sdm660-pinctrl"],
        exports: [pins::Protocol => [{ controller = "qcom,sdm660-pinctrl" }]],
    }
}

#[derive(Debug)]
enum Stopped {
    Startup(StartupError),
    Description,
    Controller,
    ServicePort,
    Executor,
    ServicePortClosed,
}

impl fmt::Display for Stopped {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Startup(error) => error.fmt(formatter),
            Self::Description => formatter.write_str("platform description refused"),
            Self::Controller => formatter.write_str("controller refused"),
            Self::ServicePort => formatter.write_str("service port refused"),
            Self::Executor => formatter.write_str("executor refused"),
            Self::ServicePortClosed => formatter.write_str("service port closed"),
        }
    }
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Stopped> {
    let requirements = PinsStartup::from_parent(parent);
    let platform = requirements.platform.open().map_err(Stopped::Startup)?;

    let executor = Rc::new(Executor::new().map_err(|_| Stopped::Executor)?);

    let platform = PortChannel::client(platform);

    let sessions = Rc::new(SessionPort::create().map_err(|_| Stopped::ServicePort)?);
    let controller = Rc::new(RefCell::new(None::<Rc<Controller>>));

    let dispatch = controller.clone();
    let bound = controller.clone();
    let closed = controller.clone();
    let port = sessions.clone();
    let spawning = Rc::downgrade(&executor);

    let served = serve_component::<pins::Protocol, _>(
        &executor,
        AsyncStartup::new(PortChannel::new(parent)),
        sessions,
        move |frame| {
            let controller = dispatch.borrow().clone();
            let controller = controller.ok_or(ipc::wire::IpcError::PeerClosed)?;
            Controller::serve(&controller, frame)
        },
        async move |_| {
            if bound.borrow().is_none() {
                let controller = bind(&platform, &port, &spawning).await.map_err(|error| {
                    log_fmt(format_args!("{COMPATIBLE} not bound: {error}"));
                    SessionError::Refused
                })?;
                *bound.borrow_mut() = Some(Rc::new(controller));
            }

            port.open_session()
        },
        move |badge| {
            if let Some(controller) = closed.borrow().as_ref() {
                controller.release(badge);
            }
        },
    );

    let _ = executor.block_on(served);
    Err(Stopped::ServicePortClosed)
}

async fn bind(
    platform: &platform::Client<PortChannel<SessionTransport>>,
    sessions: &Rc<SessionPort>,
    executor: &Weak<Executor>,
) -> Result<Controller, Stopped> {
    let region = platform
        .description()
        .await
        .map_err(|_| Stopped::Description)?;

    let mapped = MappedDescription::new(region).map_err(|_| Stopped::Description)?;
    let description = mapped.description().map_err(|_| Stopped::Description)?;
    let (tlmm, line) = take(platform, &description)
        .await
        .ok_or(Stopped::Controller)?;
    let hardware = Rc::new(RefCell::new(Hardware::new(tlmm)));
    let interrupts = executor
        .upgrade()
        .ok_or(Stopped::Executor)?
        .spawn_now(deliver(line, hardware.clone()))
        .abort_on_drop();

    Ok(Controller {
        description: mapped,
        hardware,
        claims: RefCell::new(Claims::new()),
        lines: RefCell::new(Lines::new()),
        sessions: sessions.clone(),
        _interrupts: interrupts,
    })
}

async fn deliver(line: IrqLine, hardware: Rc<RefCell<Hardware>>) {
    let mut reported = false;
    loop {
        if executor::wait(WaitItem::new(line.as_raw(), SIGNALED))
            .await
            .is_err()
        {
            log_line("summary line lost");
            exit(FAILED_EXIT_CODE);
        }

        if !hardware.borrow().deliver() && !reported {
            log_line("summary interrupt without a fired pin");
            reported = true;
        }

        if line.ack().is_err() {
            log_line("summary line lost");
            exit(FAILED_EXIT_CODE);
        }
    }
}

async fn take(
    platform: &platform::Client<PortChannel<SessionTransport>>,
    description: &Description<'_>,
) -> Option<(Tlmm<Windows>, IrqLine)> {
    let Some(id) = description
        .compatible_devices(COMPATIBLE)
        .next()
        .map(|device| device.id().clone())
    else {
        log_line("no controller described");
        return None;
    };

    let node = description.node(&id)?;
    let windows = Windows::take(platform, &id, &node)
        .await
        .inspect_err(|error| log_fmt(format_args!("{} windows refused: {error:?}", id.as_str())))
        .ok()?;

    let tlmm = Tlmm::new(windows, SDM660, description, id.clone())
        .inspect_err(|error| log_fmt(format_args!("{} refused: {error:?}", id.as_str())))
        .ok()?;
    let line = platform
        .irq(id.clone(), 0)
        .await
        .inspect_err(|error| log_fmt(format_args!("{} line refused: {error:?}", id.as_str())))
        .ok()?;

    Some((tlmm, IrqLine::from(line)))
}
