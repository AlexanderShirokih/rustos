//! Split-очередь: таблица дескрипторов, доступные и использованные элементы.
//!
//! Каждая область занимает свою страницу непрерывного региона. При размере до
//! [`QUEUE_SIZE_MAX`] область укладывается в страницу. Регион, выровненный на 16 байт,
//! выполняет требования выравнивания virtio для каждой области.

mod descriptors;

use descriptors::{Chain, Descriptors};

use crate::{
    access::{DeviceMemory, OutOfRange, Registers},
    transport::Transport,
};

const QUEUE_AREA_BYTES: usize = 4096;
/// Длина непрерывного региона очереди.
pub const QUEUE_REGION_BYTES: usize = 3 * QUEUE_AREA_BYTES;
/// Наибольший размер очереди, при котором каждая область укладывается в страницу.
pub const QUEUE_SIZE_MAX: u16 = 256;

const DESCRIPTORS_OFFSET: usize = 0;
const AVAILABLE_OFFSET: usize = QUEUE_AREA_BYTES;
const USED_OFFSET: usize = 2 * QUEUE_AREA_BYTES;
const AREA_OFFSETS: [usize; 3] = [DESCRIPTORS_OFFSET, AVAILABLE_OFFSET, USED_OFFSET];
const DESCRIPTOR_BYTES: usize = 16;
const DESCRIPTOR_ALIGNMENT: u64 = 16;
const RING_OFFSET: usize = 4;
const USED_ELEMENT_BYTES: usize = 8;
const DESCRIPTOR_FLAG_NEXT: u16 = 1;
const DESCRIPTOR_FLAG_WRITE: u16 = 2;

static ZERO_AREA: [u8; QUEUE_AREA_BYTES] = [0; QUEUE_AREA_BYTES];

/// Участок памяти, доступной устройству.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Buffer {
    /// Физический адрес начала.
    pub address: u64,
    /// Длина в байтах.
    pub length: u32,
}

/// Голова опубликованной цепочки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token(u16);

/// Цепочка, которую устройство вернуло в использованных элементах.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Completion {
    /// Голова цепочки, полученная от [`SplitQueue::submit`].
    pub token: Token,
    /// Число байт, записанных устройством в записываемые буферы цепочки.
    pub written: u32,
}

/// Отказ очереди.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueError<E> {
    /// Обращение к регистру транспорта завершилось отказом.
    Register(E),
    /// Размер после ограничения пределом устройства равен 0, не степень двойки либо
    /// больше [`QUEUE_SIZE_MAX`].
    InvalidSize(u32),
    /// Устройство уже использует очередь с этим номером.
    AlreadyActive,
    /// Физический адрес региона не выровнен на 16 байт.
    Misaligned(u64),
    /// Свободных дескрипторов меньше длины цепочки.
    Full,
    /// Цепочка не содержит ни одного буфера.
    EmptyChain,
    /// Устройство вернуло номер, не выданный как голова цепочки.
    UnknownHead(u32),
    /// Обращение выходит за границу региона очереди.
    OutOfRange,
}

/// Состояние split-очереди в процессе драйвера.
pub struct SplitQueue {
    index: u16,
    size: u16,
    descriptors: Descriptors,
    available_index: u16,
    used_index: u16,
}

impl<E> From<OutOfRange> for QueueError<E> {
    fn from(_: OutOfRange) -> Self {
        Self::OutOfRange
    }
}

impl SplitQueue {
    /// Размещает очередь `index` в регионе `memory` длиной [`QUEUE_REGION_BYTES`] и передаёт
    /// устройству адреса её областей.
    ///
    /// Размер - меньшее из `requested` и предела устройства.
    pub async fn new<R: Registers, M: DeviceMemory>(
        transport: &mut Transport<R>,
        memory: &mut M,
        index: u16,
        requested: u16,
    ) -> Result<Self, QueueError<R::Error>> {
        transport
            .select_queue(index)
            .await
            .map_err(QueueError::Register)?;
        if transport
            .queue_ready()
            .await
            .map_err(QueueError::Register)?
        {
            return Err(QueueError::AlreadyActive);
        }

        let limit = transport
            .queue_size_max()
            .await
            .map_err(QueueError::Register)?;
        let size = limit.min(u32::from(requested));
        let valid = size.is_power_of_two() && size <= u32::from(QUEUE_SIZE_MAX);
        let size = u16::try_from(size)
            .ok()
            .filter(|_| valid)
            .ok_or(QueueError::InvalidSize(size))?;
        let base = memory.physical_address();
        if !base.is_multiple_of(DESCRIPTOR_ALIGNMENT) {
            return Err(QueueError::Misaligned(base));
        }

        for area in AREA_OFFSETS {
            memory.write(area, &ZERO_AREA)?;
        }

        let [descriptors, driver, device] = AREA_OFFSETS.map(|offset| base + offset as u64);
        transport
            .configure_queue(size, descriptors, driver, device)
            .await
            .map_err(QueueError::Register)?;

        Ok(Self {
            index,
            size,
            descriptors: Descriptors::new(size),
            available_index: 0,
            used_index: 0,
        })
    }

    /// Размер очереди, согласованный с устройством.
    pub fn size(&self) -> u16 {
        self.size
    }

    /// Отправляет цепочку из буферов `readable`, которые устройство читает, и следующих за ними
    /// буферов `writable`, которые оно пишет, и уведомляет устройство.
    ///
    /// Буферы идут в цепочке в переданном порядке. Отказ уведомления не отменяет опубликованную
    /// цепочку. Её буферы остаются занятыми до завершения команды или сброса устройства.
    ///
    /// Отказы, при которых очередь не меняется:
    /// - `EmptyChain` - в цепочке нет буферов;
    /// - `Full` - свободных дескрипторов меньше числа буферов.
    pub async fn submit<R: Registers, M: DeviceMemory>(
        &mut self,
        transport: &mut Transport<R>,
        memory: &mut M,
        readable: &[Buffer],
        writable: &[Buffer],
    ) -> Result<Token, QueueError<R::Error>> {
        let buffers = readable.iter().map(|buffer| (*buffer, 0)).chain(
            writable
                .iter()
                .map(|buffer| (*buffer, DESCRIPTOR_FLAG_WRITE)),
        );
        let length = readable.len().saturating_add(writable.len());
        if length == 0 {
            return Err(QueueError::EmptyChain);
        }
        let length = u16::try_from(length).map_err(|_| QueueError::Full)?;

        let chain = self.descriptors.reserve(length)?;
        let mut descriptor = chain.head;
        for (buffer, flags) in buffers {
            if descriptor == chain.tail {
                write_descriptor(memory, descriptor, buffer, flags, 0)?;
                break;
            }
            let next = self.descriptors.link(descriptor)?;
            write_descriptor(
                memory,
                descriptor,
                buffer,
                flags | DESCRIPTOR_FLAG_NEXT,
                next,
            )?;
            descriptor = next;
        }

        self.publish(transport, memory, chain).await
    }

    /// Забирает очередную завершённую цепочку и освобождает её дескрипторы.
    pub fn take_used<R: Registers, M: DeviceMemory>(
        &mut self,
        transport: &mut Transport<R>,
        memory: &mut M,
    ) -> Result<Option<Completion>, QueueError<R::Error>> {
        let mut index = [0; 2];
        memory.read(USED_OFFSET + 2, &mut index)?;
        if u16::from_le_bytes(index) == self.used_index {
            return Ok(None);
        }
        // Элемент кольца читается после индекса, который его опубликовал.
        transport.barrier();

        let slot = usize::from(self.used_index % self.size);
        let mut element = [0; USED_ELEMENT_BYTES];
        memory.read(
            USED_OFFSET + RING_OFFSET + USED_ELEMENT_BYTES * slot,
            &mut element,
        )?;
        let [i0, i1, i2, i3, w0, w1, w2, w3] = element;
        let head = self
            .descriptors
            .release(u32::from_le_bytes([i0, i1, i2, i3]))?;
        self.used_index = self.used_index.wrapping_add(1);

        Ok(Some(Completion {
            token: Token(head),
            written: u32::from_le_bytes([w0, w1, w2, w3]),
        }))
    }

    async fn publish<R: Registers, M: DeviceMemory>(
        &mut self,
        transport: &mut Transport<R>,
        memory: &mut M,
        chain: Chain,
    ) -> Result<Token, QueueError<R::Error>> {
        let head = chain.head;
        let slot = usize::from(self.available_index % self.size);
        memory.write(
            AVAILABLE_OFFSET + RING_OFFSET + 2 * slot,
            &head.to_le_bytes(),
        )?;
        // Устройство читает элемент кольца, увидев новый индекс: элемент записан раньше.
        transport.barrier();
        let available_index = self.available_index.wrapping_add(1);
        memory.write(AVAILABLE_OFFSET + 2, &available_index.to_le_bytes())?;
        // Уведомление идёт после записи индекса, иначе устройство не увидит элемент.
        transport.barrier();
        self.descriptors.commit(chain);
        self.available_index = available_index;
        transport
            .notify(self.index)
            .await
            .map_err(QueueError::Register)?;
        Ok(Token(head))
    }
}

fn write_descriptor<M: DeviceMemory, E>(
    memory: &mut M,
    descriptor: u16,
    buffer: Buffer,
    flags: u16,
    next: u16,
) -> Result<(), QueueError<E>> {
    let mut bytes = [0; DESCRIPTOR_BYTES];
    let fields = buffer
        .address
        .to_le_bytes()
        .into_iter()
        .chain(buffer.length.to_le_bytes());
    let fields = fields.chain(flags.to_le_bytes()).chain(next.to_le_bytes());
    for (byte, field) in bytes.iter_mut().zip(fields) {
        *byte = field;
    }

    let offset = DESCRIPTORS_OFFSET + DESCRIPTOR_BYTES * usize::from(descriptor);
    memory.write(offset, &bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        testing::{
            Event, FakeMemory, FakeRegisters, Log, REGION_ADDRESS, RegisterFailure, complete,
        },
        transport::{DeviceType, register},
    };

    const REQUEST: Buffer = Buffer {
        address: 0x1000,
        length: 24,
    };
    const RESPONSE: Buffer = Buffer {
        address: 0x2000,
        length: 408,
    };

    struct Fixture {
        log: Log,
        transport: Transport<FakeRegisters>,
        memory: FakeMemory,
    }

    fn fixture(queue_limit: u32) -> Fixture {
        fixture_with(queue_limit, 0, REGION_ADDRESS)
    }

    fn fixture_with(queue_limit: u32, queue_ready: u32, address: u64) -> Fixture {
        let log = Log::default();
        let mut registers = FakeRegisters::gpu(&log);
        registers.set(register::QUEUE_SIZE_MAX, queue_limit);
        registers.set(register::QUEUE_READY, queue_ready);
        let transport = complete(Transport::probe(registers, DeviceType::Gpu)).unwrap();
        let memory = FakeMemory::new(QUEUE_REGION_BYTES, &log).at(address);
        Fixture {
            log,
            transport,
            memory,
        }
    }

    fn publish_first_used(memory: &mut FakeMemory, id: u32, written: u32) {
        publish_used(memory, &[(id, written)]);
    }

    fn publish_used(memory: &mut FakeMemory, elements: &[(u32, u32)]) {
        for (slot, (id, written)) in elements.iter().enumerate() {
            let mut element = id.to_le_bytes().to_vec();
            element.extend(written.to_le_bytes());
            memory.put_from_device(
                USED_OFFSET + RING_OFFSET + USED_ELEMENT_BYTES * slot,
                &element,
            );
        }
        let count = u16::try_from(elements.len()).unwrap();
        memory.put_from_device(USED_OFFSET + 2, &count.to_le_bytes());
    }

    #[test]
    fn queue_takes_device_limit_and_passes_area_addresses() {
        let Fixture {
            log,
            mut transport,
            mut memory,
        } = fixture(64);

        let mut queue = complete(SplitQueue::new(&mut transport, &mut memory, 0, 256)).unwrap();
        let used = queue.take_used(&mut transport, &mut memory);

        let low = |address: u64| u32::try_from(address & u64::from(u32::MAX)).unwrap();
        assert_eq!(queue.size(), 64);
        assert_eq!(log.writes_to(register::QUEUE_SIZE), [64]);
        assert_eq!(
            log.writes_to(register::QUEUE_DESCRIPTORS_LOW),
            [low(REGION_ADDRESS)]
        );
        assert_eq!(
            log.writes_to(register::QUEUE_DRIVER_LOW),
            [low(REGION_ADDRESS + 4096)]
        );
        assert_eq!(
            log.writes_to(register::QUEUE_DEVICE_LOW),
            [low(REGION_ADDRESS + 8192)]
        );
        assert_eq!(log.writes_to(register::QUEUE_DEVICE_LOW + 4), [1]);
        assert_eq!(log.writes_to(register::QUEUE_READY), [1]);
        assert_eq!(used, Ok(None));
    }

    #[test]
    fn queue_rejects_unusable_setup_before_addresses() {
        let cases = [
            (0, 0, REGION_ADDRESS, QueueError::InvalidSize(0)),
            (48, 0, REGION_ADDRESS, QueueError::InvalidSize(48)),
            (64, 1, REGION_ADDRESS, QueueError::AlreadyActive),
            (
                64,
                0,
                REGION_ADDRESS + 8,
                QueueError::Misaligned(REGION_ADDRESS + 8),
            ),
        ];

        for (limit, ready, address, expected) in cases {
            let Fixture {
                log,
                mut transport,
                mut memory,
            } = fixture_with(limit, ready, address);

            let result = complete(SplitQueue::new(&mut transport, &mut memory, 0, 256));

            assert_eq!(result.err(), Some(expected));
            assert_eq!(log.writes(), [Event::Write(register::QUEUE_SELECT, 0)]);
            assert!(
                !log.events()
                    .iter()
                    .any(|event| matches!(event, Event::MemoryWrite(..)))
            );
        }
    }

    #[test]
    fn queue_setup_stops_at_register_failure() {
        let log = Log::default();
        let registers = FakeRegisters::gpu(&log).refusing_write(register::QUEUE_DRIVER_LOW);
        let mut transport = complete(Transport::probe(registers, DeviceType::Gpu)).unwrap();
        let mut memory = FakeMemory::new(QUEUE_REGION_BYTES, &log);

        let result = complete(SplitQueue::new(&mut transport, &mut memory, 0, 2));

        assert_eq!(
            result.err(),
            Some(QueueError::Register(RegisterFailure::Write(
                register::QUEUE_DRIVER_LOW
            )))
        );
        assert!(matches!(
            log.events().last(),
            Some(Event::Write(register::QUEUE_DRIVER_LOW, _))
        ));
        assert!(log.writes_to(register::QUEUE_READY).is_empty());
    }

    #[test]
    fn failed_notification_keeps_published_descriptors_until_completion() {
        let log = Log::default();
        let registers = FakeRegisters::gpu(&log).refusing_write(register::QUEUE_NOTIFY);
        let mut transport = complete(Transport::probe(registers, DeviceType::Gpu)).unwrap();
        let mut memory = FakeMemory::new(QUEUE_REGION_BYTES, &log);
        let mut queue = complete(SplitQueue::new(&mut transport, &mut memory, 0, 2)).unwrap();

        let failed = complete(queue.submit(&mut transport, &mut memory, &[REQUEST], &[RESPONSE]));
        let occupied = complete(queue.submit(&mut transport, &mut memory, &[REQUEST], &[RESPONSE]));
        publish_first_used(&mut memory, 0, RESPONSE.length);
        let completion = queue.take_used(&mut transport, &mut memory).unwrap();

        assert_eq!(
            failed,
            Err(QueueError::Register(RegisterFailure::Write(
                register::QUEUE_NOTIFY
            )))
        );
        assert_eq!(occupied, Err(QueueError::Full));
        assert_eq!(memory.get(AVAILABLE_OFFSET + 2, 2), 1u16.to_le_bytes());
        assert_eq!(
            completion,
            Some(Completion {
                token: Token(0),
                written: RESPONSE.length
            })
        );
    }

    #[test]
    fn submit_publishes_element_then_index_then_notifies() {
        let Fixture {
            log,
            mut transport,
            mut memory,
        } = fixture(2);
        let mut queue = complete(SplitQueue::new(&mut transport, &mut memory, 3, 2)).unwrap();
        log.clear();

        complete(queue.submit(&mut transport, &mut memory, &[REQUEST], &[RESPONSE])).unwrap();

        let events = log.events();
        let tail = &events[events.len() - 5..];
        assert_eq!(
            tail,
            [
                Event::MemoryWrite(AVAILABLE_OFFSET + RING_OFFSET, vec![0, 0]),
                Event::Barrier,
                Event::MemoryWrite(AVAILABLE_OFFSET + 2, vec![1, 0]),
                Event::Barrier,
                Event::Write(register::QUEUE_NOTIFY, 3),
            ]
        );
        assert_eq!(
            memory.get(DESCRIPTORS_OFFSET + 8, 8),
            [24, 0, 0, 0, 1, 0, 1, 0]
        );
        assert_eq!(
            memory.get(DESCRIPTORS_OFFSET + 24, 6),
            [0x98, 1, 0, 0, 2, 0]
        );
    }

    #[test]
    fn a_virtio_block_read_chains_one_readable_and_two_writable_descriptors() {
        let Fixture {
            mut transport,
            mut memory,
            ..
        } = fixture(4);
        let mut queue = complete(SplitQueue::new(&mut transport, &mut memory, 0, 4)).unwrap();
        let status = Buffer {
            address: 0x3000,
            length: 1,
        };

        let token =
            complete(queue.submit(&mut transport, &mut memory, &[REQUEST], &[RESPONSE, status]))
                .unwrap();
        let chained: Vec<_> = (0..3)
            .map(|descriptor| {
                memory
                    .get(DESCRIPTORS_OFFSET + DESCRIPTOR_BYTES * descriptor + 12, 4)
                    .to_vec()
            })
            .collect();
        publish_first_used(&mut memory, u32::from(token.0), 1);
        queue.take_used(&mut transport, &mut memory).unwrap();
        let refilled =
            complete(queue.submit(&mut transport, &mut memory, &[REQUEST], &[RESPONSE, status]));
        let last = complete(queue.submit(&mut transport, &mut memory, &[status], &[]));
        let overflow = complete(queue.submit(&mut transport, &mut memory, &[status], &[]));

        assert_eq!(chained, [[1, 0, 1, 0], [3, 0, 2, 0], [2, 0, 0, 0]]);
        assert!(refilled.is_ok());
        assert!(last.is_ok());
        assert_eq!(overflow, Err(QueueError::Full));
    }

    #[test]
    fn an_empty_chain_is_refused_without_touching_the_queue() {
        let Fixture {
            log,
            mut transport,
            mut memory,
        } = fixture(2);
        let mut queue = complete(SplitQueue::new(&mut transport, &mut memory, 0, 2)).unwrap();
        log.clear();

        let result = complete(queue.submit(&mut transport, &mut memory, &[], &[]));

        assert_eq!(result, Err(QueueError::EmptyChain));
        assert!(log.events().is_empty());
        assert_eq!(memory.get(AVAILABLE_OFFSET + 2, 2), [0, 0]);
    }

    #[test]
    fn submit_to_full_queue_changes_nothing() {
        for (size, submitted) in [(1, 0u16), (2, 1)] {
            let Fixture {
                log,
                mut transport,
                mut memory,
            } = fixture(size);
            let mut queue = complete(SplitQueue::new(&mut transport, &mut memory, 0, 2)).unwrap();
            for _ in 0..submitted {
                complete(queue.submit(&mut transport, &mut memory, &[REQUEST], &[RESPONSE]))
                    .unwrap();
            }

            log.clear();

            let result =
                complete(queue.submit(&mut transport, &mut memory, &[REQUEST], &[RESPONSE]));

            assert_eq!(result, Err(QueueError::Full));
            assert!(log.events().is_empty());
            assert_eq!(memory.get(AVAILABLE_OFFSET + 2, 2), submitted.to_le_bytes());
        }
    }

    #[test]
    fn completion_reads_element_after_index_and_frees_chain() {
        let Fixture {
            log,
            mut transport,
            mut memory,
        } = fixture(2);
        let mut queue = complete(SplitQueue::new(&mut transport, &mut memory, 0, 2)).unwrap();
        let token =
            complete(queue.submit(&mut transport, &mut memory, &[REQUEST], &[RESPONSE])).unwrap();
        publish_first_used(&mut memory, u32::from(token.0), 408);
        log.clear();

        let completion = queue.take_used(&mut transport, &mut memory).unwrap();
        let read_order = log.events();
        let again = queue.take_used(&mut transport, &mut memory).unwrap();
        let resubmitted =
            complete(queue.submit(&mut transport, &mut memory, &[REQUEST], &[RESPONSE]));

        assert_eq!(
            completion,
            Some(Completion {
                token,
                written: 408
            })
        );
        assert_eq!(
            read_order,
            [
                Event::MemoryRead(USED_OFFSET + 2),
                Event::Barrier,
                Event::MemoryRead(USED_OFFSET + RING_OFFSET),
            ]
        );
        assert_eq!(again, None);
        assert_eq!(resubmitted, Ok(token));
    }

    #[test]
    fn used_element_with_foreign_id_is_rejected() {
        let Fixture {
            mut transport,
            mut memory,
            ..
        } = fixture(4);
        let mut queue = complete(SplitQueue::new(&mut transport, &mut memory, 0, 4)).unwrap();
        complete(queue.submit(&mut transport, &mut memory, &[REQUEST], &[RESPONSE])).unwrap();
        publish_first_used(&mut memory, 1, 0);

        let result = queue.take_used(&mut transport, &mut memory);

        assert_eq!(result, Err(QueueError::UnknownHead(1)));
    }
}
