//! Устройство virtio-blk за слотом шины: подключение и обмен запросами.
//!
//! Из признаков устройства драйвер согласует `BLK_SIZE` и `SIZE_MAX`. QEMU запускает устройство
//! без согласованных `FLUSH` и `CONFIG_WCE` в режиме сквозной записи и завершает запрос записи
//! после сброса данных в образ диска на хосте (`hw/block/virtio-blk.c`, `virtio_blk_set_status`).

use block_abi::BlockInfo;
use driver_virtio_blk::{Channel, GeometryError, geometry};
use ipc::CallError;
use kernel_objects::{IrqLine, SessionTransport};
use runtime::{Timeout, log_fmt};
use syscall::PAGE_SIZE;
use virtio::{
    Buffer, DeviceMemory, DeviceType, QUEUE_REGION_BYTES, QueueError, SplitQueue, Transport,
    TransportError,
    block::{Config, HEADER_BYTES, REQUEST_QUEUE, RequestKind, encode_header, feature},
};
use virtio_driver::{
    CompletionFailure, DmaRegion, SlotFailure, SlotRegisters, await_completion, open_slot,
};
use virtio_mmio_abi::{SlotError, VirtioMmioError, virtio_mmio};

const SLOT_CALL_TIMEOUT: Timeout = Timeout::from_secs(2);
const COMPLETION_TIMEOUT: Timeout = Timeout::from_secs(2);

const REQUEST_QUEUE_SIZE: u16 = 4;

const STATUS_OFFSET: usize = HEADER_BYTES;
const STATUS_BYTES: u32 = 1;
const STATUS_UNWRITTEN: u8 = 0xFF;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConnectRefusal {
    Bus(CallError<VirtioMmioError>),
    Line(CallError<SlotError>),
    Transport(TransportError<SlotFailure>),
    Geometry(GeometryError),
    Queue(QueueError<SlotFailure>),
    Slot(SlotFailure),
    Memory(kernel_objects::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Failure {
    Queue(QueueError<SlotFailure>),
    Completion(CompletionFailure),
    Memory,
    UnexpectedCompletion,
}

pub(crate) struct Device {
    transport: Transport<SlotRegisters>,
    line: IrqLine,
    queue: SplitQueue,
    queue_memory: DmaRegion,
    header: DmaRegion,
}

impl Device {
    pub(crate) fn connect(
        bus: &virtio_mmio::Client<SessionTransport>,
    ) -> Result<(Self, BlockInfo), ConnectRefusal> {
        let slot = open_slot(bus, DeviceType::Block)
            .map_err(ConnectRefusal::Bus)?
            .client()
            .with_wait(SLOT_CALL_TIMEOUT);
        let line = IrqLine::from(slot.irq().map_err(ConnectRefusal::Line)?);
        let mut transport = Transport::probe(SlotRegisters::new(slot), DeviceType::Block)
            .map_err(ConnectRefusal::Transport)?;
        let accepted = transport
            .negotiate(feature::BLK_SIZE | feature::SIZE_MAX)
            .map_err(ConnectRefusal::Transport)?;
        let config = Config::read(&transport).map_err(ConnectRefusal::Slot)?;
        let info = geometry(accepted, config).map_err(ConnectRefusal::Geometry)?;

        let header = DmaRegion::new(PAGE_SIZE as u64).map_err(ConnectRefusal::Memory)?;
        let mut queue_memory =
            DmaRegion::new(QUEUE_REGION_BYTES as u64).map_err(ConnectRefusal::Memory)?;
        let queue = SplitQueue::new(
            &mut transport,
            &mut queue_memory,
            REQUEST_QUEUE,
            REQUEST_QUEUE_SIZE,
        )
        .map_err(ConnectRefusal::Queue)?;
        if let Err(failure) = transport.start() {
            // Несброшенное устройство вправе обращаться к очереди до конца процесса.
            if transport.reset().is_err() {
                core::mem::forget(queue_memory);
            }
            return Err(ConnectRefusal::Slot(failure));
        }

        let device = Self {
            transport,
            line,
            queue,
            queue_memory,
            header,
        };
        Ok((device, info))
    }

    fn exchange(
        &mut self,
        kind: RequestKind,
        sector: u64,
        region: &DmaRegion,
        bytes: u32,
    ) -> Result<u8, Failure> {
        self.header
            .write(0, &encode_header(kind, sector))
            .map_err(|_| Failure::Memory)?;
        // Статус, который устройство не записало, не совпадает ни с одним значением статуса.
        self.header
            .write(STATUS_OFFSET, &[STATUS_UNWRITTEN])
            .map_err(|_| Failure::Memory)?;

        let base = self.header.physical_address();
        let header = Buffer {
            address: base,
            length: u32::try_from(HEADER_BYTES).map_err(|_| Failure::Memory)?,
        };
        let status = Buffer {
            address: base + STATUS_OFFSET as u64,
            length: STATUS_BYTES,
        };
        let data = Buffer {
            address: region.physical_address(),
            length: bytes,
        };
        let (readable, writable): (&[Buffer], &[Buffer]) = match kind {
            RequestKind::Read => (&[header], &[data, status]),
            RequestKind::Write => (&[header, data], &[status]),
        };
        let token = self
            .queue
            .submit(
                &mut self.transport,
                &mut self.queue_memory,
                readable,
                writable,
            )
            .map_err(Failure::Queue)?;

        let completion = await_completion(
            &mut self.queue,
            &mut self.transport,
            &mut self.queue_memory,
            &self.line,
            COMPLETION_TIMEOUT,
        )
        .map_err(Failure::Completion)?;
        if completion.token != token {
            return Err(Failure::UnexpectedCompletion);
        }
        let mut status = [STATUS_UNWRITTEN];
        self.header
            .read(STATUS_OFFSET, &mut status)
            .map_err(|_| Failure::Memory)?;
        let [status] = status;
        Ok(status)
    }
}

impl Channel for Device {
    type Region = DmaRegion;

    fn transfer(
        &mut self,
        kind: RequestKind,
        sector: u64,
        region: &DmaRegion,
        bytes: u32,
    ) -> Option<u8> {
        self.exchange(kind, sector, region, bytes)
            .inspect_err(|failure| {
                log_fmt(format_args!(
                    "request to sector {sector} failed: {failure:?}"
                ));
            })
            .ok()
    }

    fn reset(&mut self) -> bool {
        self.transport
            .reset()
            .inspect_err(|failure| log_fmt(format_args!("device reset failed: {failure:?}")))
            .is_ok()
    }
}
