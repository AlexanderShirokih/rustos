//! Драйвер virtio-input. Предоставляет протокол `InputSource`: `attach` настраивает свободные
//! транспорты virtio-input шины `VirtioMmio` и объявляет в сессии `InputInject` устройство
//! каждого настроенного транспорта, затем драйвер передаёт в неё кадры событий устройств.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::{sync::Arc, vec::Vec};
use core::{convert::Infallible, fmt};

use component_startup::{
    SessionEvent, SessionPort, StartupError, component_manifest, serve_export, whole_export,
};
use driver_virtio_input::Device;
use input_abi::{AttachError, DeviceNumber, RecordsError, UpdateError, input_inject, input_source};
use ipc::CallError;
use kernel_objects::{Delivery, IrqLine, ReceivedSession, SessionTransport};
use runtime::{Timeout, log_fmt, spawn_bound};
use syscall::PAGE_SIZE;
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
    ExportThread,
    ServicePortClosed,
    NoDevice,
    Input(DeviceNumber, Failure),
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
    bus: virtio_mmio::Client<SessionTransport>,
    attached: bool,
    refused: bool,
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
            Self::ExportThread => formatter.write_str("export thread refused"),
            Self::ServicePortClosed => formatter.write_str("service port closed"),
            Self::NoDevice => formatter.write_str("no input device attached"),
            Self::Input(id, failure) => write!(formatter, "input device {id}: {failure:?}"),
        }
    }
}

impl input_source::Service<Delivery> for Source {
    fn attach(
        &mut self,
        _frame: &Delivery,
        inject: ReceivedSession<input_inject::Protocol>,
    ) -> Result<(), AttachError> {
        if self.attached {
            return Err(AttachError::Busy);
        }
        self.attached = true;
        let inject = kernel_objects::Session::from_session(inject)
            .client()
            .with_wait(CALL_TIMEOUT);
        let result = self.attach_inputs(&Arc::new(inject));
        // Отказ до ответа хабу завершает процесс после ответа `Refused`.
        self.refused = result.is_err();
        result
    }
}

impl Source {
    fn attach_inputs(
        &self,
        inject: &Arc<input_inject::Client<SessionTransport>>,
    ) -> Result<(), AttachError> {
        // Шина отвечает `NoDevice` либо `Busy`, когда свободных транспортов ввода не остаётся.
        let mut slots = Vec::new();
        while let Ok(slot) = open_slot(&self.bus, DeviceType::Input) {
            slots.push(slot);
        }
        let mut inputs = Vec::new();
        for slot in slots {
            match Input::open(slot.client().with_wait(CALL_TIMEOUT)) {
                Ok(input) => inputs.push(input),
                Err(failure) => log_fmt(format_args!("input transport refused: {failure:?}")),
            }
        }
        if inputs.is_empty() {
            return Err(AttachError::Refused);
        }
        for input in inputs {
            let id = input
                .device
                .descriptor()
                .map(|descriptor| inject.declare(descriptor));
            let Some(Ok(id)) = id else {
                log_fmt(format_args!("declare refused: {id:?}"));
                return Err(AttachError::Refused);
            };
            log_fmt(format_args!(
                "input device {id} attached: {}",
                input.device.name()
            ));
            let inject = Arc::clone(inject);
            spawn_bound(move || {
                let Err(failure) = input.serve(id, &inject);
                Stopped::Input(id, failure)
            })
            .map_err(|_| AttachError::Refused)?;
        }
        Ok(())
    }
}

impl Input {
    fn open(slot: virtio_slot::Client<SessionTransport>) -> Result<Self, Failure> {
        let line = IrqLine::from(slot.irq().map_err(Failure::Irq)?);
        let mut transport = Transport::probe(SlotRegisters::new(slot), DeviceType::Input)
            .map_err(Failure::Transport)?;
        transport.negotiate(0).map_err(Failure::Transport)?;
        let mut queue_memory =
            DmaRegion::new(QUEUE_REGION_BYTES as u64).map_err(Failure::Memory)?;
        let queue = SplitQueue::new(
            &mut transport,
            &mut queue_memory,
            EVENT_QUEUE,
            EVENT_BUFFERS,
        )
        .map_err(Failure::Queue)?;
        transport.start().map_err(Failure::Slot)?;
        let device = Device::describe(&mut transport).map_err(Failure::Slot)?;
        let mut input = Self {
            transport,
            line,
            queue,
            queue_memory,
            events: DmaRegion::new(PAGE_SIZE as u64).map_err(Failure::Memory)?,
            buffers: Vec::new(),
            device,
        };
        for index in 0..usize::from(input.queue.size()) {
            let token = input.offer(index)?;
            input.buffers.push(token);
        }
        Ok(input)
    }

    fn serve(
        mut self,
        id: DeviceNumber,
        inject: &input_inject::Client<SessionTransport>,
    ) -> Result<Infallible, Failure> {
        loop {
            self.drain(id, inject)?;
            self.line.wait(Timeout::INFINITE).map_err(Failure::Line)?;
            let causes = self.transport.interrupt_status().map_err(Failure::Slot)?;
            self.transport
                .acknowledge_interrupt(causes)
                .map_err(Failure::Slot)?;
            self.line.ack().map_err(Failure::Line)?;
        }
    }

    fn drain(
        &mut self,
        id: DeviceNumber,
        inject: &input_inject::Client<SessionTransport>,
    ) -> Result<(), Failure> {
        while let Some(done) = self
            .queue
            .take_used(&mut self.transport, &mut self.queue_memory)
            .map_err(Failure::Queue)?
        {
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
            let token = self.offer(index)?;
            if let Some(buffer) = self.buffers.get_mut(index) {
                *buffer = token;
            }

            // Буфер короче события устройство не заполнило, и событие пропускается.
            let Ok(event) = event else { continue };
            if let Some(command) = self.device.translate(event).map_err(Failure::Records)? {
                inject
                    .update(id, None, command.as_bytes())
                    .map_err(Failure::Update)?;
            }
        }
        Ok(())
    }

    fn offer(&mut self, index: usize) -> Result<Token, Failure> {
        let buffer = Buffer {
            address: self.events.physical_address() + (index * EVENT_BYTES) as u64,
            length: u32::try_from(EVENT_BYTES).map_err(|_| Failure::EventBuffer)?,
        };
        self.queue
            .offer(&mut self.transport, &mut self.queue_memory, buffer)
            .map_err(Failure::Queue)
    }
}

#[program_entry::component]
fn run(requirements: VirtioInputStartup) -> Result<(), Stopped> {
    let bus = requirements
        .virtio
        .open()
        .map_err(Stopped::Startup)?
        .client()
        .with_wait(CALL_TIMEOUT);
    let export = SessionPort::create().map_err(|_| Stopped::ServicePort)?;
    let export = Arc::new(export);

    let parent = requirements.parent;
    let port = Arc::clone(&export);
    let _ =
        spawn_bound(move || serve_export::<input_source::Protocol>(parent, whole_export(&port)))
            .map_err(|_| Stopped::ExportThread)?;

    let mut source = Source {
        bus,
        attached: false,
        refused: false,
    };
    let mut events = export.events();

    while let Some(event) = events.recv() {
        if let SessionEvent::Frame(frame) = event {
            let _ = input_source::dispatch(&mut source, frame);
            if source.refused {
                return Err(Stopped::NoDevice);
            }
        }
    }

    Err(Stopped::ServicePortClosed)
}
