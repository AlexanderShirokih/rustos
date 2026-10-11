//! Таблица устройств и сессии `Input`.
//!
//! Каждая запись добавляется в очереди после изменения таблицы. Очередь, которая не вмещает
//! запись, заменяется последовательностью начала сессии по текущей таблице.

use alloc::{boxed::Box, collections::BTreeMap};

use component_startup::{MintedSession, SessionError, SessionPort};
use input_abi::{
    DeclareError, DescribeError, DeviceDescriptor, DeviceLayout, MESSAGE_RECORDS_MAX, NextError,
    RECORD_SIZE, Record, RecordBuf, Tracker, UpdateError, input, input_inject,
};
use kernel_objects::{Delivery, IssuedHandle, Port, kind};
use runtime::{Instant, Mutex};

use crate::queue::{QUEUE_RECORDS_MAX, Queue};

const INPUT_SESSIONS_MAX: usize = 16;

const START_RECORDS_LIMIT: usize = QUEUE_RECORDS_MAX - MESSAGE_RECORDS_MAX;

type Debt = IssuedHandle<kind::Reply>;
type Raw = [u8; RECORD_SIZE];

static HUB: Mutex<Hub> = Mutex::new(Hub {
    devices: BTreeMap::new(),
    sessions: BTreeMap::new(),
    next_device: Some(1),
    start_records_max: 1,
});

struct Hub {
    devices: BTreeMap<u16, Box<Device>>,
    sessions: BTreeMap<u64, Session>,
    next_device: Option<u16>,
    start_records_max: usize,
}

struct Device {
    owner: u64,
    tracker: Tracker,
    snapshot_max: usize,
}

struct Session {
    queue: Queue,
    waiting: Option<input::next::Reply<Debt>>,
}

pub(crate) struct Source;

pub(crate) struct Consumer;

impl Hub {
    fn broadcast(&mut self, records: &[Raw]) {
        let Self {
            devices, sessions, ..
        } = self;
        for session in sessions.values_mut() {
            if session.queue.fits(records.len()) {
                session.queue.push(records);
            } else {
                restart(&mut session.queue, devices);
            }
            session.answer();
        }
    }

    fn broadcast_one(&mut self, record: Record) {
        self.broadcast(&[record.to_bytes()]);
    }

    fn remove(&mut self, number: u16) {
        let Some(device) = self.devices.get_mut(&number) else {
            return;
        };
        let mut cancel = RecordBuf::new();
        device
            .tracker
            .cancel(Instant::now().as_nanos(), &mut cancel);
        if !cancel.is_empty() {
            self.broadcast(as_raw(&cancel));
        }

        if let Some(device) = self.devices.remove(&number) {
            self.start_records_max -= 1 + device.snapshot_max;
        }
        self.broadcast_one(Record::device_removed(number));
    }
}

impl Session {
    fn answer(&mut self) {
        if self.queue.is_empty() {
            return;
        }

        if let Some(reply) = self.waiting.take() {
            let _ = reply.send(Ok::<_, NextError>(self.queue.take_message()));
        }
    }
}

impl input_inject::Service<Delivery> for Source {
    fn declare(
        &mut self,
        frame: &Delivery,
        descriptor: DeviceDescriptor<'_>,
    ) -> Result<u16, DeclareError> {
        let layout = descriptor.layout()?;
        let snapshot_max = layout.snapshot_records_max();

        let mut hub = HUB.lock();
        let start_records_max = hub.start_records_max + 1 + snapshot_max;
        if start_records_max > START_RECORDS_LIMIT {
            return Err(DeclareError::NoSpace);
        }

        let number = hub.next_device.ok_or(DeclareError::NoSpace)?;
        hub.next_device = number.checked_add(1);
        let device = Device {
            owner: frame.badge,
            tracker: Tracker::new(number, layout),
            snapshot_max,
        };
        hub.devices.insert(number, Box::new(device));
        hub.start_records_max = start_records_max;
        hub.broadcast_one(Record::device_added(number));
        Ok(number)
    }

    fn update(
        &mut self,
        frame: &Delivery,
        device: u16,
        time: Option<u64>,
        records: &[u8],
    ) -> Result<(), UpdateError> {
        let now = Instant::now().as_nanos();
        let mut hub = HUB.lock();
        let owned = hub
            .devices
            .get_mut(&device)
            .filter(|owned| owned.owner == frame.badge)
            .ok_or(UpdateError::UnknownDevice)?;

        let mut command = RecordBuf::new();
        owned.tracker.apply(records, time, now, &mut command)?;
        hub.broadcast(as_raw(&command));
        Ok(())
    }
}

impl input::Service<Delivery> for Consumer {
    fn next(&mut self, frame: &Delivery, reply: input::next::Reply<Debt>) {
        let mut hub = HUB.lock();
        // Без сессии долг разрушается, и вызыватель получает `PeerClosed`.
        let Some(session) = hub.sessions.get_mut(&frame.badge) else {
            return;
        };

        if session.waiting.is_some() {
            drop(hub);
            let _ = reply.send(Err::<RecordBuf, _>(NextError::Busy));
            return;
        }

        session.waiting = Some(reply);
        session.answer();
    }

    fn describe(&mut self, _frame: &Delivery, device: u16) -> Result<DeviceLayout, DescribeError> {
        HUB.lock()
            .devices
            .get(&device)
            .map(|device| device.tracker.layout().clone())
            .ok_or(DescribeError::UnknownDevice)
    }
}

pub(crate) fn open_source(export: &SessionPort) -> Result<Port, SessionError> {
    let MintedSession { port, .. } = export.create_session().map_err(|_| SessionError::NoSpace)?;
    Ok(port)
}

pub(crate) fn open_input(export: &SessionPort) -> Result<Port, SessionError> {
    let mut hub = HUB.lock();
    if hub.sessions.len() >= INPUT_SESSIONS_MAX {
        return Err(SessionError::NoSpace);
    }

    let mut queue = Queue::new().ok_or(SessionError::NoSpace)?;
    restart(&mut queue, &hub.devices);
    let MintedSession { badge, port } =
        export.create_session().map_err(|_| SessionError::NoSpace)?;
    let session = Session {
        queue,
        waiting: None,
    };
    hub.sessions.insert(badge.get(), session);
    Ok(port)
}

pub(crate) fn source_gone(badge: u64) {
    let mut hub = HUB.lock();
    while let Some(number) = hub
        .devices
        .iter()
        .find(|(_, device)| device.owner == badge)
        .map(|(&number, _)| number)
    {
        hub.remove(number);
    }
}

pub(crate) fn input_gone(badge: u64) {
    // Разрушенный долг ожидающего `next` даёт ушедшему клиенту `PeerClosed`.
    HUB.lock().sessions.remove(&badge);
}

fn restart(queue: &mut Queue, devices: &BTreeMap<u16, Box<Device>>) {
    queue.clear();
    queue.push(&[Record::resync().to_bytes()]);
    let mut snapshot = RecordBuf::new();
    for (&number, device) in devices {
        queue.push(&[Record::device_added(number).to_bytes()]);
        device.tracker.snapshot(&mut snapshot);
        queue.push(as_raw(&snapshot));
    }
}

fn as_raw(records: &RecordBuf) -> &[Raw] {
    records.as_bytes().as_chunks().0
}
