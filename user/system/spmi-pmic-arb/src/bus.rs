//! Сессии `Spmi` и сессии устройств выпускаются на одном порту из общего счётчика значков.
//! Кадр значка, которым занято устройство, обслуживает протокол `SpmiDevice`, остальные
//! кадры - протокол `Spmi`.

use alloc::vec::Vec;
use core::sync::atomic::AtomicU64;

use component_startup::mint_session;
use ipc::{IntoWireValue, Listener, RecvError, wire::MESSAGE_INLINE_MAX};
use kernel_objects::{Delivery, Port, Signal};
use runtime::{Mutex, PortTransport, Timeout, log_fmt};
use spmi_abi::{COMPATIBLE_MAX, DeviceError, SpmiError, spmi, spmi_device};
use spmi_pmic_arb::{Arbiter, Interrupt, Peripheral};
use syscall::{Rights, SIGNALED, WakeCount};

use crate::windows::Windows;

const SIGNAL_RIGHTS: Rights = Rights::from_bits_truncate(
    Rights::READ.bits() | Rights::DUPLICATE.bits() | Rights::TRANSFER.bits(),
);

pub(crate) struct Sessions {
    pub(crate) service: Port,
    pub(crate) next_badge: AtomicU64,
}

pub(crate) struct Bus {
    pub(crate) arbiter: Option<Arbiter<Windows>>,
    pub(crate) devices: Vec<Device>,
}

pub(crate) struct Device {
    pub(crate) peripheral: Peripheral,
    pub(crate) holder: Option<Holder>,
}

pub(crate) struct Holder {
    badge: u64,
    leases: Vec<Lease>,
}

struct Lease {
    resource: u16,
    interrupt: Interrupt,
    signal: Signal,
}

struct Opener<'a> {
    sessions: &'a Sessions,
    bus: &'a mut Bus,
}

struct Held<'a> {
    arbiter: &'a Arbiter<Windows>,
    holder: &'a mut Holder,
    peripheral: &'a Peripheral,
}

impl Bus {
    pub(crate) fn deliver(&self) {
        let Some(arbiter) = &self.arbiter else {
            return;
        };

        arbiter.drain(|ppid, bits| {
            let fired = self.leases().filter(|lease| {
                lease.interrupt.ppid == ppid && bits & (1 << lease.interrupt.bit) != 0
            });
            for lease in fired {
                let _ = lease.signal.notify_all();
            }
        });
    }

    fn release(&mut self, badge: u64) {
        // Сброс `holder` закрывает `Signal` его прерываний и отзывает их копии у клиента.
        let Some(holder) = self
            .devices
            .iter_mut()
            .find_map(|device| device.holder.take_if(|holder| holder.badge == badge))
        else {
            return;
        };

        let Some(arbiter) = &self.arbiter else {
            return;
        };

        for lease in &holder.leases {
            let _ = arbiter.mask(lease.interrupt.ppid, 1 << lease.interrupt.bit);
        }

        for lease in &holder.leases {
            let shared = self
                .leases()
                .any(|other| other.interrupt.ppid == lease.interrupt.ppid);

            if !shared {
                arbiter.disable(lease.interrupt.ppid);
            }
        }
    }

    fn leases(&self) -> impl Iterator<Item = &Lease> {
        self.devices
            .iter()
            .filter_map(|device| device.holder.as_ref())
            .flat_map(|holder| holder.leases.iter())
    }
}

impl Opener<'_> {
    fn open(&mut self, compatible: &str) -> Result<Port, SpmiError> {
        let mut matching = self
            .bus
            .devices
            .iter_mut()
            .filter(|device| device.peripheral.is_compatible(compatible))
            .peekable();

        if matching.peek().is_none() {
            return Err(SpmiError::NoDevice);
        }

        let device = matching
            .find(|device| device.holder.is_none())
            .ok_or(SpmiError::Busy)?;

        let minted = mint_session(&self.sessions.service, &self.sessions.next_badge)
            .map_err(|_| SpmiError::NoSpace)?;

        device.holder = Some(Holder {
            badge: minted.badge.get(),
            leases: Vec::new(),
        });

        for ppid in device.peripheral.ppids() {
            let owner = self
                .bus
                .arbiter
                .as_ref()
                .and_then(|arbiter| arbiter.owner(ppid));

            log_fmt(format_args!(
                "{} channel {ppid:#05x} owner {owner:?}",
                device.peripheral.id().as_str()
            ));
        }

        Ok(minted.port)
    }
}

impl spmi::Service<Delivery> for Opener<'_> {
    fn open(
        &mut self,
        _frame: &Delivery,
        compatible: ipc::wire::Str<'_, COMPATIBLE_MAX>,
    ) -> impl IntoWireValue<Delivery, Read = Result<ipc::wire::Session<spmi_device::Protocol>, SpmiError>>
    {
        // Порт сессии устройства выпускает эта шина, и она же обслуживает на нём `SpmiDevice`.
        Opener::open(self, compatible.as_str()).map(kernel_objects::Session::from_handle)
    }
}

impl Held<'_> {
    fn lease(&self, resource: u16) -> Option<&Lease> {
        self.holder
            .leases
            .iter()
            .find(|lease| lease.resource == resource)
    }
}

impl spmi_device::Service<Delivery> for Held<'_> {
    fn read(&mut self, _frame: &Delivery, address: u16, count: u8) -> Result<u64, DeviceError> {
        let span = self.peripheral.span(address, count)?;
        self.arbiter.read(span.ppid, span.offset, count)
    }

    fn write(
        &mut self,
        _frame: &Delivery,
        address: u16,
        count: u8,
        value: u64,
    ) -> Result<(), DeviceError> {
        let span = self.peripheral.span(address, count)?;
        self.arbiter.write(span.ppid, span.offset, count, value)
    }

    fn irq(
        &mut self,
        _frame: &Delivery,
        resource: u16,
    ) -> impl IntoWireValue<Delivery, Read = Result<ipc::wire::Cap<syscall::kind::Signal>, DeviceError>>
    {
        let interrupt = self.peripheral.interrupt(resource)?;

        if let Some(lease) = self.lease(resource) {
            return lease
                .signal
                .duplicate(SIGNAL_RIGHTS, None)
                .map_err(|_| DeviceError::Unavailable);
        }

        let signal = Signal::create().map_err(|_| DeviceError::Unavailable)?;
        let copy = signal
            .duplicate(SIGNAL_RIGHTS, None)
            .map_err(|_| DeviceError::Unavailable)?;

        self.arbiter.arm(&interrupt)?;
        self.holder.leases.push(Lease {
            resource,
            interrupt,
            signal,
        });

        Ok(copy)
    }

    fn ack(&mut self, _frame: &Delivery, resource: u16) -> Result<(), DeviceError> {
        let interrupt = self.peripheral.interrupt(resource)?;
        let lease = self.lease(resource).ok_or(DeviceError::Unavailable)?;

        lease
            .signal
            .set(0, SIGNALED, WakeCount::None)
            .map_err(|_| DeviceError::Unavailable)?;

        self.arbiter.rearm(&interrupt)
    }
}

pub(crate) fn serve(bus: &Mutex<Bus>, sessions: &Sessions) {
    let transport = PortTransport::new(sessions.service.borrow());
    let mut bytes = [0u8; MESSAGE_INLINE_MAX];

    // Отброшенный кадр порт не закрывает, а закрытие порта терминально.
    loop {
        match transport.recv(&mut bytes, Timeout::INFINITE) {
            Ok(frame) => {
                let badge = frame.resources.badge;
                let mut bus = bus.lock();
                let Bus { arbiter, devices } = &mut *bus;

                let held = arbiter.as_ref().and_then(|arbiter| {
                    devices.iter_mut().find_map(|device| {
                        let holder = device
                            .holder
                            .as_mut()
                            .filter(|holder| holder.badge == badge)?;

                        Some(Held {
                            arbiter,
                            holder,
                            peripheral: &device.peripheral,
                        })
                    })
                });

                if let Some(mut held) = held {
                    let _ = spmi_device::dispatch(&mut held, frame);
                } else {
                    let mut opener = Opener {
                        sessions,
                        bus: &mut bus,
                    };
                    let _ = spmi::dispatch(&mut opener, frame);
                }
            }
            Err(RecvError::SenderGone(badge)) => bus.lock().release(badge),
            Err(RecvError::Discarded | RecvError::Timeout) => {}
            Err(RecvError::Closed) => break,
        }
    }
}
