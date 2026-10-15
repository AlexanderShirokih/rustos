//! Таблица устройств, сессии `Input` и фокус.
//!
//! Каждая запись добавляется после изменения таблицы в очереди сессий, которым видно её
//! устройство.

use alloc::{
    boxed::Box,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};
use core::cell::RefCell;

use component_startup::{MintedSession, SessionError, SessionPort};
use input_abi::{
    DeclareError, DescribeError, DeviceDescriptor, DeviceLayout, DeviceNumber, FocusError,
    FocusNumber, FocusSession, MESSAGE_RECORDS_MAX, NextError, RECORD_SIZE, Record, RecordBuf,
    Tracker, UpdateError, input, input_focus, input_inject,
};
use ipc::IntoWireValue;
use kernel_objects::{Delivery, IssuedHandle, Port, kind};
use runtime::Instant;

use crate::queue::{QUEUE_RECORDS_MAX, Queue};

const INPUT_SESSIONS_MAX: usize = 16;

const START_RECORDS_LIMIT: usize = QUEUE_RECORDS_MAX - MESSAGE_RECORDS_MAX;

type Debt = IssuedHandle<kind::Reply>;
type Raw = [u8; RECORD_SIZE];
type Devices = BTreeMap<DeviceNumber, Box<Device>>;

pub(crate) struct Hub {
    devices: Devices,
    sessions: BTreeMap<u64, Session>,
    holding: Option<Holding>,
    next_device: Option<DeviceNumber>,
    next_app: Option<FocusNumber>,
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

struct Holding {
    badge: u64,
    apps: BTreeMap<FocusNumber, App>,
    focused: Option<FocusNumber>,
    reserved: BTreeSet<DeviceNumber>,
}

struct App {
    badge: u64,
    session: Session,
}

pub(crate) struct Source {
    pub(crate) hub: Rc<RefCell<Hub>>,
}

pub(crate) struct Consumer {
    pub(crate) hub: Rc<RefCell<Hub>>,
}

pub(crate) struct Holder {
    pub(crate) hub: Rc<RefCell<Hub>>,
    pub(crate) consumers: &'static SessionPort,
}

impl Hub {
    pub(crate) const fn new() -> Self {
        Self {
            devices: BTreeMap::new(),
            sessions: BTreeMap::new(),
            holding: None,
            next_device: Some(DeviceNumber::FIRST),
            next_app: Some(FocusNumber::FIRST),
            start_records_max: 1,
        }
    }

    fn broadcast(&mut self, device: DeviceNumber, records: &[Raw]) {
        let Self {
            devices,
            sessions,
            holding,
            ..
        } = self;

        for session in sessions.values_mut() {
            session.deliver(records, devices, |_| true);
        }

        if let Some(holding) = holding
            && !holding.reserved.contains(&device)
        {
            holding.deliver(records, devices);
        }
    }

    fn remove(&mut self, number: DeviceNumber) {
        let Some(device) = self.devices.get_mut(&number) else {
            return;
        };
        let mut cancel = RecordBuf::new();
        device
            .tracker
            .cancel(Instant::now().as_nanos(), &mut cancel);
        if !cancel.is_empty() {
            self.broadcast(number, as_raw(&cancel));
        }

        if let Some(device) = self.devices.remove(&number) {
            self.start_records_max -= 1 + device.snapshot_max;
        }
        self.broadcast(number, &[Record::device_removed(number).to_bytes()]);
        // Резерв снимается после рассылки, поэтому сессии фокуса, не видевшие устройство, не
        // получают его `DeviceRemoved`.
        if let Some(holding) = &mut self.holding {
            holding.reserved.remove(&number);
        }
    }

    fn session_mut(&mut self, badge: u64) -> Option<&mut Session> {
        self.sessions.get_mut(&badge).or_else(|| {
            self.holding
                .as_mut()?
                .apps
                .values_mut()
                .find(|app| app.badge == badge)
                .map(|app| &mut app.session)
        })
    }

    fn shows(&self, badge: u64, device: DeviceNumber) -> bool {
        if self.sessions.contains_key(&badge) {
            return true;
        }

        self.holding.as_ref().is_some_and(|holding| {
            holding
                .focused
                .and_then(|number| holding.apps.get(&number))
                .is_some_and(|app| app.badge == badge)
                && !holding.reserved.contains(&device)
        })
    }

    fn sessions_len(&self) -> usize {
        self.sessions.len()
            + self
                .holding
                .as_ref()
                .map_or(0, |holding| holding.apps.len())
    }
}

impl Session {
    fn start(devices: &Devices, shows: impl Fn(DeviceNumber) -> bool) -> Option<Self> {
        let mut session = Self {
            queue: Queue::new()?,
            waiting: None,
        };

        session.restart(devices, shows);

        Some(session)
    }

    fn deliver(
        &mut self,
        records: &[Raw],
        devices: &Devices,
        shows: impl Fn(DeviceNumber) -> bool,
    ) {
        if self.queue.fits(records.len()) {
            self.queue.push(records);
        } else {
            self.restart(devices, shows);
        }
        self.answer();
    }

    fn restart(&mut self, devices: &Devices, shows: impl Fn(DeviceNumber) -> bool) {
        self.queue.clear();
        self.queue.push(&[Record::resync().to_bytes()]);

        let mut snapshot = RecordBuf::new();
        for (&number, device) in devices {
            if shows(number) {
                self.queue.push(&[Record::device_added(number).to_bytes()]);
                device.tracker.snapshot(&mut snapshot);
                self.queue.push(as_raw(&snapshot));
            }
        }
    }

    fn answer(&mut self) {
        if self.queue.is_empty() {
            return;
        }

        if let Some(reply) = self.waiting.take() {
            let _ = reply.send(Ok::<_, NextError>(self.queue.take_message()));
        }
    }
}

impl Holding {
    fn deliver(&mut self, records: &[Raw], devices: &Devices) {
        let Self {
            apps,
            focused,
            reserved,
            ..
        } = self;

        if let Some(app) = focused.and_then(|number| apps.get_mut(&number)) {
            app.session
                .deliver(records, devices, |device| !reserved.contains(&device));
        }
    }

    fn refocus(&mut self, number: Option<FocusNumber>, devices: &Devices) {
        let previous = core::mem::replace(&mut self.focused, number);
        if previous == number {
            return;
        }

        if let Some(app) = previous.and_then(|previous| self.apps.get_mut(&previous)) {
            app.session.restart(devices, |_| false);
            app.session.answer();
        }

        let reserved = &self.reserved;
        if let Some(app) = number.and_then(|number| self.apps.get_mut(&number)) {
            app.session
                .restart(devices, |device| !reserved.contains(&device));
            app.session.answer();
        }
    }

    fn reserve(&mut self, device: DeviceNumber, devices: &Devices) {
        if self.reserved.insert(device) {
            self.deliver(&[Record::device_removed(device).to_bytes()], devices);
        }
    }

    fn close(&mut self, badge: u64) {
        let Some(number) = self
            .apps
            .iter()
            .find(|(_, app)| app.badge == badge)
            .map(|(&number, _)| number)
        else {
            return;
        };

        self.apps.remove(&number);
        if self.focused == Some(number) {
            self.focused = None;
        }
    }
}

impl Source {
    pub(crate) fn gone(&self, badge: u64) {
        let mut hub = self.hub.borrow_mut();
        while let Some(number) = hub
            .devices
            .iter()
            .find(|(_, device)| device.owner == badge)
            .map(|(&number, _)| number)
        {
            hub.remove(number);
        }
    }
}

impl Consumer {
    pub(crate) fn gone(&self, badge: u64) {
        let mut hub = self.hub.borrow_mut();
        if hub.sessions.remove(&badge).is_none()
            && let Some(holding) = &mut hub.holding
        {
            holding.close(badge);
        }
    }
}

impl Holder {
    pub(crate) fn gone(&self, badge: u64) {
        let mut hub = self.hub.borrow_mut();
        if hub
            .holding
            .as_ref()
            .is_some_and(|holding| holding.badge == badge)
        {
            hub.holding = None;
        }
    }
}

impl input_inject::Service<Delivery> for Source {
    fn declare(
        &mut self,
        frame: &Delivery,
        descriptor: DeviceDescriptor<'_>,
    ) -> Result<DeviceNumber, DeclareError> {
        let layout = descriptor.layout()?;
        let snapshot_max = layout.snapshot_records_max();

        let mut hub = self.hub.borrow_mut();
        let start_records_max = hub.start_records_max + 1 + snapshot_max;
        if start_records_max > START_RECORDS_LIMIT {
            return Err(DeclareError::NoSpace);
        }

        let number = hub.next_device.ok_or(DeclareError::NoSpace)?;
        hub.next_device = number.next();
        let device = Device {
            owner: frame.badge,
            tracker: Tracker::new(number, layout),
            snapshot_max,
        };
        hub.devices.insert(number, Box::new(device));
        hub.start_records_max = start_records_max;
        hub.broadcast(number, &[Record::device_added(number).to_bytes()]);
        Ok(number)
    }

    fn update(
        &mut self,
        frame: &Delivery,
        device: DeviceNumber,
        time: Option<u64>,
        records: &[u8],
    ) -> Result<(), UpdateError> {
        let now = Instant::now().as_nanos();
        let mut hub = self.hub.borrow_mut();
        let owned = hub
            .devices
            .get_mut(&device)
            .filter(|owned| owned.owner == frame.badge)
            .ok_or(UpdateError::UnknownDevice)?;

        let mut command = RecordBuf::new();
        owned.tracker.apply(records, time, now, &mut command)?;
        hub.broadcast(device, as_raw(&command));
        Ok(())
    }
}

impl input::Service<Delivery> for Consumer {
    fn next(&mut self, frame: &Delivery, reply: input::next::Reply<Debt>) {
        let mut hub = self.hub.borrow_mut();
        // Без сессии долг разрушается, и вызыватель получает `PeerClosed`.
        let Some(session) = hub.session_mut(frame.badge) else {
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

    fn describe(
        &mut self,
        frame: &Delivery,
        device: DeviceNumber,
    ) -> Result<DeviceLayout, DescribeError> {
        let hub = self.hub.borrow();
        if !hub.shows(frame.badge, device) {
            return Err(DescribeError::UnknownDevice);
        }

        hub.devices
            .get(&device)
            .map(|device| device.tracker.layout().clone())
            .ok_or(DescribeError::UnknownDevice)
    }
}

impl input_focus::Service<Delivery> for Holder {
    fn open(
        &mut self,
        frame: &Delivery,
    ) -> impl IntoWireValue<Delivery, Read = Result<FocusSession, FocusError>> {
        let mut hub = self.hub.borrow_mut();
        let total = hub.sessions_len();
        let Hub {
            devices,
            holding,
            next_app,
            ..
        } = &mut *hub;
        let holding = claim(holding, frame.badge)?;
        if total >= INPUT_SESSIONS_MAX {
            return Err(FocusError::NoSpace);
        }

        let number = next_app.ok_or(FocusError::NoSpace)?;
        let session = Session::start(devices, |_| false).ok_or(FocusError::NoSpace)?;
        let MintedSession { badge, port } = self
            .consumers
            .create_session()
            .map_err(|_| FocusError::NoSpace)?;

        *next_app = number.next();
        holding.apps.insert(
            number,
            App {
                badge: badge.get(),
                session,
            },
        );

        Ok(FocusSession {
            session: kernel_objects::Session::from_handle(port),
            number,
        })
    }

    fn focus(&mut self, frame: &Delivery, session: Option<FocusNumber>) -> Result<(), FocusError> {
        let mut hub = self.hub.borrow_mut();
        let Hub {
            devices, holding, ..
        } = &mut *hub;
        let holding = claim(holding, frame.badge)?;

        if let Some(number) = session
            && !holding.apps.contains_key(&number)
        {
            return Err(FocusError::UnknownSession);
        }

        holding.refocus(session, devices);
        Ok(())
    }

    fn reserve(&mut self, frame: &Delivery, device: DeviceNumber) -> Result<(), FocusError> {
        let mut hub = self.hub.borrow_mut();
        let Hub {
            devices, holding, ..
        } = &mut *hub;
        let holding = claim(holding, frame.badge)?;
        if !devices.contains_key(&device) {
            return Err(FocusError::UnknownDevice);
        }

        holding.reserve(device, devices);
        Ok(())
    }
}

pub(crate) fn open_source(export: &SessionPort) -> Result<Port, SessionError> {
    let MintedSession { port, .. } = export.create_session().map_err(|_| SessionError::NoSpace)?;
    Ok(port)
}

pub(crate) fn open_input(hub: &RefCell<Hub>, export: &SessionPort) -> Result<Port, SessionError> {
    let mut hub = hub.borrow_mut();
    if hub.sessions_len() >= INPUT_SESSIONS_MAX {
        return Err(SessionError::NoSpace);
    }

    let session = Session::start(&hub.devices, |_| true).ok_or(SessionError::NoSpace)?;
    let MintedSession { badge, port } =
        export.create_session().map_err(|_| SessionError::NoSpace)?;
    hub.sessions.insert(badge.get(), session);

    Ok(port)
}

fn claim(holding: &mut Option<Holding>, badge: u64) -> Result<&mut Holding, FocusError> {
    let holding = holding.get_or_insert_with(|| Holding {
        badge,
        apps: BTreeMap::new(),
        focused: None,
        reserved: BTreeSet::new(),
    });

    if holding.badge == badge {
        Ok(holding)
    } else {
        Err(FocusError::Busy)
    }
}

fn as_raw(records: &RecordBuf) -> &[Raw] {
    records.as_bytes().as_chunks().0
}
