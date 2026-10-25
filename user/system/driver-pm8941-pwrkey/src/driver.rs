//! Источник ввода `InputSource` одной кнопки PON: подключение по описанию платформы и передача
//! нажатий хабу по прерыванию кнопки.

use alloc::rc::{Rc, Weak};
use core::{cell::Cell, fmt};

use component_startup::{
    AnyOf, AsyncStartup, Import, Providers, SessionPort, StartupError, serve_component,
    whole_export,
};
use executor::{Executor, Mutex, PortChannel, Reactor};
use input_abi::{
    AttachError, AxisList, Binding, Command, DeclareError, DeviceDescriptor, DeviceNumber, KeyList,
    UpdateError, Usage, input_inject, input_source,
};
use ipc::CallError;
use kernel_objects::{Delivery, ReceivedSession, Session, SessionTransport, Signal};
use platform_abi::{CONTROLLER_KEY, DescriptionError, MappedDescription, PlatformError, platform};
use runtime::{FAILED_EXIT_CODE, Instant, Timeout, exit, log_fmt};
use spmi_abi::{DeviceError, SpmiError, spmi, spmi_device, spmi_interrupt};
use syscall::{Handle, SIGNALED, WaitItem, kind};

use crate::key::{DEBOUNCE_DEFAULT_US, Generation, Key, Transition, transition, usage};

type Channel = PortChannel<SessionTransport>;

const SUBTYPE: u16 = 0x05;
const RT_STS: u16 = 0x10;
const PULL_CTL: u16 = 0x70;
const DBC_CTL: u16 = 0x71;

const KEY_INTERRUPT: u16 = 0;
const KEY_INDEX: u16 = 0;

const CALL_TIMEOUT: Timeout = Timeout::from_secs(2);

/// Причина остановки программы кнопки.
#[derive(Debug)]
pub enum Stopped {
    /// Требование запуска не выдано.
    Startup(StartupError),
    /// Порт экспорта не создан.
    ExportPort,
    /// Executor не создан.
    Executor,
    /// Порт экспорта закрыт.
    ExportPortClosed,
}

struct Source {
    key: &'static Key,
    spmi: Providers<spmi::Protocol>,
    platform: platform::Client<Channel>,
    attached: Mutex<bool>,
    refused: Cell<bool>,
    executor: Weak<Executor>,
    reactor: Reactor,
}

struct Node {
    bus: spmi::Client<Channel>,
    device: spmi_device::Client<Channel>,
    base: u16,
    usage: Usage,
    debounce: u32,
    pull_up: bool,
}

struct Button {
    _bus: spmi::Client<Channel>,
    device: spmi_device::Client<Channel>,
    inject: input_inject::Client<Channel>,
    number: DeviceNumber,
    interrupt: spmi_interrupt::Client<Channel>,
    signal: Signal,
    status: u16,
    status_bit: u8,
    window: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Failure {
    Platform(CallError<PlatformError>),
    Mapping(kernel_objects::Error),
    Description(DescriptionError),
    NoNode,
    NoBase,
    NoController,
    NoBus,
    Startup(StartupError),
    UnknownCode(u32),
    Debounce(u32),
    Bus(CallError<SpmiError>),
    Device(CallError<DeviceError>),
    Descriptor,
    Declare(CallError<DeclareError>),
    Update(CallError<UpdateError>),
    Signal(kernel_objects::Error),
    Executor,
}

impl fmt::Display for Stopped {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Startup(error) => error.fmt(formatter),
            Self::ExportPort => formatter.write_str("export port refused"),
            Self::Executor => formatter.write_str("executor refused"),
            Self::ExportPortClosed => formatter.write_str("export port closed"),
        }
    }
}

impl Source {
    async fn bind(&self, inject: input_inject::Client<Channel>) -> Result<(), Failure> {
        let node = Node::find(self.key, &self.platform, &self.spmi, &self.reactor).await?;
        let button = Button::bind(self.key, node, inject, &self.reactor).await?;

        self.executor
            .upgrade()
            .ok_or(Failure::Executor)?
            .spawn(button.watch(self.reactor.clone()));

        Ok(())
    }
}

impl input_source::Service<Delivery> for Source {
    async fn attach(
        &self,
        _frame: &Delivery,
        inject: ReceivedSession<input_inject::Protocol>,
    ) -> Result<(), AttachError> {
        let mut attached = self.attached.lock().await;
        if *attached {
            return Err(AttachError::Busy);
        }

        let inject = PortChannel::client(Session::from_session(inject), self.reactor.clone())
            .with_wait(CALL_TIMEOUT);

        match self.bind(inject).await {
            Ok(()) => {
                log_fmt(format_args!("{} bound", self.key.compatible));
                *attached = true;
                Ok(())
            }

            Err(failure) => {
                log_fmt(format_args!(
                    "{} not bound: {failure:?}",
                    self.key.compatible
                ));
                self.refused.set(true);
                Err(AttachError::Refused)
            }
        }
    }
}

impl Node {
    async fn find(
        key: &Key,
        platform: &platform::Client<Channel>,
        spmi: &Providers<spmi::Protocol>,
        reactor: &Reactor,
    ) -> Result<Self, Failure> {
        let description = platform.description().await.map_err(Failure::Platform)?;
        let mapped = MappedDescription::new(description)
            .map_err(|refused| Failure::Mapping(refused.error))?;
        let description = mapped.description().map_err(Failure::Description)?;

        let id = description
            .compatible_devices(key.compatible)
            .next()
            .map(|device| device.id().clone())
            .ok_or(Failure::NoNode)?;
        let node = description.node(&id).ok_or(Failure::NoNode)?;
        let compatible = spmi_abi::controller(&description, &id)
            .ok_or(Failure::NoController)?
            .strings("compatible")
            .map_err(Failure::Description)?
            .ok_or(Failure::NoController)?;

        let session = spmi
            .open_first(reactor, AnyOf::new(CONTROLLER_KEY, compatible))
            .await
            .map_err(Failure::Startup)?
            .ok_or(Failure::NoBus)?;

        let bus = PortChannel::client(session, reactor.clone()).with_wait(CALL_TIMEOUT);

        let device = PortChannel::client(
            Session::<spmi_device::Protocol>::from_session(
                bus.open(id.clone()).await.map_err(Failure::Bus)?,
            ),
            reactor.clone(),
        )
        .with_wait(CALL_TIMEOUT);

        // База блока PON - первая ячейка `reg` узла, а без него - родителя, как в Linux.
        let parent = description.ancestors(&id).next();
        let reg = match node.cells("reg").map_err(Failure::Description)? {
            Some(reg) => Some(reg),
            None => parent
                .map(|parent| parent.node.cells("reg"))
                .transpose()
                .map_err(Failure::Description)?
                .flatten(),
        };
        let base = reg
            .and_then(|mut reg| reg.next())
            .and_then(|base| u16::try_from(base).ok())
            .ok_or(Failure::NoBase)?;

        let code = node.u32("linux,code").map_err(Failure::Description)?;
        let usage = usage(code).map_err(Failure::UnknownCode)?;

        let debounce = node
            .u32("debounce")
            .map_err(Failure::Description)?
            .unwrap_or(DEBOUNCE_DEFAULT_US);

        Ok(Self {
            bus,
            device,
            base,
            usage,
            debounce,
            pull_up: node.bytes("bias-pull-up").is_some(),
        })
    }
}

impl Button {
    async fn bind(
        key: &Key,
        node: Node,
        inject: input_inject::Client<Channel>,
        reactor: &Reactor,
    ) -> Result<Self, Failure> {
        let Node {
            bus,
            device,
            base,
            usage,
            debounce,
            pull_up,
        } = node;
        let register = |offset: u16| base.wrapping_add(offset);

        let subtype = device.read(register(SUBTYPE), 1).await?;
        let generation = Generation::of(u8::try_from(subtype).unwrap_or_default());
        let mask = generation.mask();
        let field = generation
            .debounce_field(debounce)
            .ok_or(Failure::Debounce(debounce))?;

        device.update(register(DBC_CTL), mask, field).await?;
        let pull_up = if pull_up { key.pull_up_bit } else { 0 };
        device
            .update(register(PULL_CTL), key.pull_up_bit, pull_up)
            .await?;

        // Поле общее для кнопок блока, и окно считается по записанному последним значению.
        let configured = device.read(register(DBC_CTL), 1).await?;
        let window = generation.debounce_window(u8::try_from(configured).unwrap_or_default());

        let line = device.irq(KEY_INTERRUPT).await?;
        let interrupt = PortChannel::client(
            Session::<spmi_interrupt::Protocol>::from_session(line),
            reactor.clone(),
        )
        .with_wait(CALL_TIMEOUT);

        let signal = Signal::from(interrupt.signal().await?);

        let mut keys = KeyList::new();
        keys.push(usage).map_err(|_| Failure::Descriptor)?;
        let axes = AxisList::new();
        let descriptor = DeviceDescriptor::new(key.name, Binding::None, &keys, &axes, 0, &axes)
            .ok_or(Failure::Descriptor)?;

        let number = inject.declare(descriptor).await.map_err(Failure::Declare)?;

        Ok(Self {
            _bus: bus,
            device,
            inject,
            number,
            interrupt,
            signal,
            status: register(RT_STS),
            status_bit: key.status_bit,
            window,
        })
    }

    async fn watch(self, reactor: Reactor) {
        let mut pressed = false;
        let mut quiet_until = 0;

        loop {
            if let Err(error) = reactor
                .wait(WaitItem::new(self.signal.as_raw(), SIGNALED))
                .await
            {
                stop(&Failure::Signal(error));
            }

            if Instant::now().as_nanos() >= quiet_until {
                match self.report(pressed).await {
                    Ok((set, time)) => {
                        pressed = set;
                        if !set {
                            quiet_until = time.saturating_add(self.window);
                        }
                    }

                    Err(failure) => stop(&failure),
                }
            }

            if let Err(error) = self.interrupt.ack().await {
                stop(&Failure::Device(error));
            }
        }
    }

    async fn report(&self, pressed: bool) -> Result<(bool, u64), Failure> {
        let status = self.device.read(self.status, 1).await?;
        let time = Instant::now().as_nanos();
        let set = status & u64::from(self.status_bit) != 0;

        let steps: &[bool] = match transition(pressed, set) {
            Transition::None => &[],
            Transition::Press => &[true],
            Transition::Release => &[false],
            Transition::PressAndRelease => &[true, false],
        };

        for down in steps {
            let mut command = Command::new();
            let built = if *down {
                command.press(KEY_INDEX)
            } else {
                command.release(KEY_INDEX)
            };

            built.map_err(|_| Failure::Descriptor)?;

            self.inject
                .update(self.number, Some(time), command.as_bytes())
                .await
                .map_err(Failure::Update)?;
        }

        Ok((set, time))
    }
}

impl From<CallError<DeviceError>> for Failure {
    fn from(error: CallError<DeviceError>) -> Self {
        Self::Device(error)
    }
}

/// Обслуживает экспорт `InputSource` кнопки `key` до закрытия порта экспорта. Отказ
/// подключения либо обслуживания кнопки завершает процесс.
pub fn run(
    key: &'static Key,
    spmi: Providers<spmi::Protocol>,
    platform: &Import<platform::Protocol>,
    parent: Handle<kind::Port>,
) -> Result<(), Stopped> {
    let executor = Rc::new(Executor::new().map_err(|_| Stopped::Executor)?);
    let reactor = executor.reactor();

    let platform = PortChannel::client(platform.open().map_err(Stopped::Startup)?, reactor.clone())
        .with_wait(CALL_TIMEOUT);

    let export = Rc::new(SessionPort::create().map_err(|_| Stopped::ExportPort)?);
    let source = Rc::new(Source {
        key,
        spmi,
        platform,
        attached: Mutex::new(false),
        refused: Cell::new(false),
        executor: Rc::downgrade(&executor),
        reactor: reactor.clone(),
    });

    let opened = export.clone();

    let served = serve_component::<input_source::Protocol, _>(
        &executor,
        AsyncStartup::new(PortChannel::new(parent, reactor)),
        export,
        move |frame| {
            let call = input_source::serve(&source, frame)?;
            let source = source.clone();
            Ok(call.map(|call| async move {
                let outcome = call.await;
                if source.refused.get() {
                    exit(FAILED_EXIT_CODE);
                }
                outcome
            }))
        },
        whole_export(opened),
        |_| {},
    );

    let _ = executor.block_on(served);
    Err(Stopped::ExportPortClosed)
}

fn stop(failure: &Failure) -> ! {
    log_fmt(format_args!("button stopped: {failure:?}"));
    exit(FAILED_EXIT_CODE)
}
