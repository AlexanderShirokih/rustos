use alloc::{
    boxed::Box,
    rc::{Rc, Weak},
    vec::Vec,
};
use core::{
    cell::{Cell, RefCell},
    fmt,
};

use component_startup::{
    AsyncStartup, Requirements as _, SessionPort, StartupError, component_manifest,
    serve_component, whole_export,
};
use driver_virtio_input::Device;
use executor::{Executor, JoinHandle, Mutex, PortChannel, Reactor};
use input_abi::{AttachError, DeviceNumber, RecordsError, UpdateError, input_inject, input_source};
use ipc::CallError;
use kernel_objects::{Delivery, IrqLine, ReceivedSession, Session, SessionTransport, Signal};
use runtime::{FAILED_EXIT_CODE, Timeout, exit, log_fmt};
use syscall::{Handle, PAGE_SIZE, SIGNALED, WaitItem, kind};
use virtio::{
    Buffer, DeviceMemory, DeviceType, QUEUE_REGION_BYTES, QueueError, SplitQueue, Token, Transport,
    TransportError,
    input::{EVENT_BYTES, EVENT_QUEUE, Event},
};
use virtio_driver::{DmaRegion, SlotFailure, SlotRegisters, open_slot};
use virtio_mmio_abi::{SlotError, virtio_mmio, virtio_slot};

const CALL_TIMEOUT: Timeout = Timeout::from_secs(2);

const EVENT_BUFFERS: u16 = 64;

component_manifest! {
    struct VirtioInputStartup {
        requirements: { virtio: session(virtio_mmio::Protocol) },
        features: ["virtio,mmio"],
        exports: [input_source::Protocol],
    }
}

#[derive(Debug)]
enum Stopped {
    Startup(StartupError),
    ServicePort,
    Executor,
    ServicePortClosed,
}

#[derive(Debug, PartialEq)]
enum Failure {
    Irq(CallError<SlotError>),
    Line(kernel_objects::Error),
    Slot(SlotFailure),
    Transport(TransportError<SlotFailure>),
    Queue(QueueError<SlotFailure>),
    Memory(kernel_objects::Error),
    EventBuffer,
    Records(RecordsError),
    Update(CallError<UpdateError>),
}

struct Source {
    owner: Weak<Source>,
    bus: virtio_mmio::Client<PortChannel<SessionTransport>>,
    attached: Mutex<bool>,
    refused: Cell<bool>,
    executor: Weak<Executor>,
    reactor: Reactor,
    shutdown: Rc<Signal>,
    workers: RefCell<Vec<JoinHandle<()>>>,
}

struct Input {
    transport: Transport<SlotRegisters>,
    line: IrqLine,
    queue: SplitQueue,
    queue_memory: DmaRegion,
    events: DmaRegion,
    buffers: Vec<Token>,
    device: Device,
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

impl input_source::Service<Delivery> for Source {
    async fn attach(
        &self,
        _frame: &Delivery,
        inject: ReceivedSession<input_inject::Protocol>,
    ) -> Result<(), AttachError> {
        let task = {
            let executor = self.executor.upgrade().ok_or(AttachError::Refused)?;
            let source = self.owner.upgrade().ok_or(AttachError::Refused)?;
            executor.spawn(async move {
                let mut attached = source.attached.lock().await;
                if *attached {
                    return Err(AttachError::Busy);
                }

                *attached = true;

                let inject =
                    PortChannel::client(Session::from_session(inject), source.reactor.clone())
                        .with_wait(CALL_TIMEOUT);

                let result = Box::pin(source.attach_inputs(&Rc::new(inject))).await;
                source.refused.set(result.is_err());
                result
            })
        };

        task.await.ok_or(AttachError::Refused)?
    }
}

impl Source {
    async fn attach_inputs(
        &self,
        inject: &Rc<input_inject::Client<PortChannel<SessionTransport>>>,
    ) -> Result<(), AttachError> {
        let mut slots = Vec::new();
        while let Ok(slot) = open_slot(&self.bus, &self.reactor, DeviceType::Input).await {
            slots.push(slot);
        }

        let mut attached = false;

        for slot in slots {
            let slot = PortChannel::client(slot, self.reactor.clone()).with_wait(CALL_TIMEOUT);
            let input = match Box::pin(Input::open(slot)).await {
                Ok(input) => input,
                Err(failure) => {
                    log_fmt(format_args!("input transport refused: {failure:?}"));
                    continue;
                }
            };

            let declared = match input.device.descriptor() {
                Some(descriptor) => inject.declare(descriptor).await.ok(),
                None => None,
            };

            let Some(id) = declared else {
                input.release().await;
                return Err(AttachError::Refused);
            };

            log_fmt(format_args!(
                "input device {id} attached: {}",
                input.device.name()
            ));

            let task = {
                let Some(executor) = self.executor.upgrade() else {
                    input.release().await;
                    return Err(AttachError::Refused);
                };

                let inject = inject.clone();
                let reactor = self.reactor.clone();
                let shutdown = self.shutdown.clone();
                executor.spawn(async move {
                    let mut input = input;
                    let outcome = input.serve(id, &inject, &reactor, &shutdown).await;
                    input.release().await;
                    if let Err(failure) = outcome {
                        log_fmt(format_args!("input device {id}: {failure:?}"));
                        exit(FAILED_EXIT_CODE);
                    }
                })
            };

            self.workers.borrow_mut().push(task);
            attached = true;
        }

        if attached {
            Ok(())
        } else {
            Err(AttachError::Refused)
        }
    }

    async fn stop(&self) {
        let _attached = self.attached.lock().await;
        let _ = self.shutdown.notify_all();

        let workers = core::mem::take(&mut *self.workers.borrow_mut());

        for worker in workers {
            let _ = worker.await;
        }
    }
}

impl Input {
    async fn open(
        slot: virtio_slot::Client<PortChannel<SessionTransport>>,
    ) -> Result<Self, Failure> {
        let line = IrqLine::from(slot.irq().await.map_err(Failure::Irq)?);
        let mut transport = Transport::probe(SlotRegisters::new(slot), DeviceType::Input)
            .await
            .map_err(Failure::Transport)?;

        transport.negotiate(0).await.map_err(Failure::Transport)?;
        let device = Device::describe(&mut transport)
            .await
            .map_err(Failure::Slot)?;

        let events = DmaRegion::new(PAGE_SIZE as u64).map_err(Failure::Memory)?;
        let mut queue_memory =
            DmaRegion::new(QUEUE_REGION_BYTES as u64).map_err(Failure::Memory)?;

        let configured = async {
            let queue = SplitQueue::new(
                &mut transport,
                &mut queue_memory,
                EVENT_QUEUE,
                EVENT_BUFFERS,
            )
            .await
            .map_err(Failure::Queue)?;
            transport.start().await.map_err(Failure::Slot)?;
            Ok(queue)
        }
        .await;
        let queue = match configured {
            Ok(queue) => queue,
            Err(failure) => {
                if transport.reset().await.is_err() {
                    core::mem::forget(queue_memory);
                }

                return Err(failure);
            }
        };

        let mut input = Self {
            transport,
            line,
            queue,
            queue_memory,
            events,
            buffers: Vec::new(),
            device,
        };

        for index in 0..usize::from(input.queue.size()) {
            match input.offer(index).await {
                Ok(token) => input.buffers.push(token),
                Err(failure) => {
                    input.release().await;
                    return Err(failure);
                }
            }
        }

        Ok(input)
    }

    async fn release(mut self) {
        if self.transport.reset().await.is_err() {
            core::mem::forget((self.queue_memory, self.events));
        }
    }

    async fn serve(
        &mut self,
        id: DeviceNumber,
        inject: &input_inject::Client<PortChannel<SessionTransport>>,
        reactor: &Reactor,
        shutdown: &Signal,
    ) -> Result<(), Failure> {
        let waits = [
            WaitItem::new(shutdown.as_raw(), SIGNALED),
            WaitItem::new(self.line.as_raw(), SIGNALED),
        ];

        loop {
            self.drain(id, inject).await?;
            if reactor.wait_any(&waits).await.map_err(Failure::Line)? == 0 {
                return Ok(());
            }

            let causes = self
                .transport
                .interrupt_status()
                .await
                .map_err(Failure::Slot)?;
            self.transport
                .acknowledge_interrupt(causes)
                .await
                .map_err(Failure::Slot)?;
            self.line.ack().map_err(Failure::Line)?;
        }
    }

    async fn drain(
        &mut self,
        id: DeviceNumber,
        inject: &input_inject::Client<PortChannel<SessionTransport>>,
    ) -> Result<(), Failure> {
        for _ in 0..self.queue.size() {
            let Some(done) = self
                .queue
                .take_used(&mut self.transport, &mut self.queue_memory)
                .map_err(Failure::Queue)?
            else {
                break;
            };

            let index = self
                .buffers
                .iter()
                .position(|token| *token == done.token)
                .ok_or(Failure::EventBuffer)?;

            let mut bytes = [0; EVENT_BYTES];
            let written = usize::try_from(done.written).unwrap_or(0).min(EVENT_BYTES);
            let event = bytes.get_mut(..written).unwrap_or_default();
            self.events
                .read(index * EVENT_BYTES, event)
                .map_err(|_| Failure::EventBuffer)?;

            let event = Event::parse(event);

            let token = self.offer(index).await?;
            if let Some(buffer) = self.buffers.get_mut(index) {
                *buffer = token;
            }

            // Буфер короче события устройство не заполнило, и событие пропускается.
            let Ok(event) = event else { continue };
            for command in self.device.translate(event).map_err(Failure::Records)? {
                inject
                    .update(id, None, command.as_bytes())
                    .await
                    .map_err(Failure::Update)?;
            }
        }

        Ok(())
    }

    async fn offer(&mut self, index: usize) -> Result<Token, Failure> {
        let buffer = Buffer {
            address: self.events.physical_address() + (index * EVENT_BYTES) as u64,
            length: u32::try_from(EVENT_BYTES).map_err(|_| Failure::EventBuffer)?,
        };

        self.queue
            .submit(&mut self.transport, &mut self.queue_memory, &[], &[buffer])
            .await
            .map_err(Failure::Queue)
    }
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Stopped> {
    let requirements = VirtioInputStartup::from_parent(parent);
    let bus = requirements.virtio.open().map_err(Stopped::Startup)?;

    let executor = Rc::new(Executor::new().map_err(|_| Stopped::Executor)?);
    let reactor = executor.reactor();

    let export = Rc::new(SessionPort::create().map_err(|_| Stopped::ServicePort)?);
    let shutdown = Rc::new(Signal::create().map_err(|_| Stopped::Executor)?);

    let source = Rc::new_cyclic(|owner| Source {
        owner: owner.clone(),
        bus: PortChannel::client(bus, reactor.clone()).with_wait(CALL_TIMEOUT),
        attached: Mutex::new(false),
        refused: Cell::new(false),
        executor: Rc::downgrade(&executor),
        reactor: reactor.clone(),
        shutdown,
        workers: RefCell::new(Vec::new()),
    });

    let dispatch = source.clone();
    let opened = export.clone();

    let served = serve_component::<input_source::Protocol, _>(
        &executor,
        AsyncStartup::new(PortChannel::new(parent, reactor)),
        export,
        move |frame| {
            let call = input_source::serve(&dispatch, frame)?;
            let source = dispatch.clone();
            Ok(call.map(|call| async move {
                let outcome = call.await;
                if source.refused.get() {
                    source.stop().await;
                    exit(FAILED_EXIT_CODE);
                }
                outcome
            }))
        },
        whole_export(opened),
        |_| {},
    );

    executor.block_on(async {
        let _ = served.await;
        source.stop().await;
    });
    Err(Stopped::ServicePortClosed)
}
