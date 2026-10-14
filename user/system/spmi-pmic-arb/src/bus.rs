//! Сессии `Spmi`, сессии устройств и сессии прерываний выпускаются на одном порту из общего
//! счётчика значков. Кадр значка, которым занято устройство, обслуживает протокол `SpmiDevice`,
//! кадр значка аренды прерывания либо сессии прерывания ушедшего устройства - протокол
//! `SpmiInterrupt`, остальные кадры - протокол `Spmi`.

use alloc::vec::Vec;

use component_startup::{SessionEvent, SessionPort};
use ipc::IntoWireValue;
use kernel_objects::{Delivery, Port, Signal};
use platform_abi::DeviceId;
use runtime::{Mutex, log_fmt};
use spmi_abi::{DeviceError, SpmiError, Trigger, spmi, spmi_device, spmi_interrupt};
use spmi_pmic_arb::{Arbiter, Interrupt, Peripheral};
use syscall::{Rights, SIGNALED, WakeCount};

use crate::windows::Windows;

const SIGNAL_RIGHTS: Rights = Rights::READ
    .union(Rights::DUPLICATE)
    .union(Rights::TRANSFER);

pub(crate) struct Bus {
    pub(crate) arbiter: Option<Arbiter<Windows>>,
    pub(crate) devices: Vec<Device>,
    pub(crate) departed_interrupts: Vec<u64>,
}

pub(crate) struct Device {
    pub(crate) peripheral: Peripheral,
    pub(crate) holder: Option<Holder>,
}

pub(crate) struct Holder {
    badge: u64,
    peripheral: Peripheral,
    leases: Vec<Lease>,
}

struct Lease {
    badge: u64,
    interrupt: Interrupt,
    signal: Signal,
}

struct Opener<'a> {
    sessions: &'a SessionPort,
    bus: &'a mut Bus,
}

struct Held<'a> {
    sessions: &'a SessionPort,
    arbiter: &'a Arbiter<Windows>,
    holder: &'a mut Holder,
}

struct InterruptSession<'a>(Option<(&'a Arbiter<Windows>, &'a Lease)>);

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
        // Сброс аренды закрывает её `Signal` и отзывает его копии у клиентов.
        if let Some(holder) = self
            .devices
            .iter_mut()
            .find_map(|device| device.holder.take_if(|holder| holder.badge == badge))
        {
            self.departed_interrupts
                .extend(holder.leases.iter().map(|lease| lease.badge));
            self.revoke(&holder.leases);
        } else if let Some(lease) = self.devices.iter_mut().find_map(|device| {
            let leases = &mut device.holder.as_mut()?.leases;
            let index = leases.iter().position(|lease| lease.badge == badge)?;
            Some(leases.remove(index))
        }) {
            self.revoke(core::slice::from_ref(&lease));
        } else {
            self.departed_interrupts
                .retain(|departed| *departed != badge);
        }
    }

    fn revoke(&self, revoked: &[Lease]) {
        if let Some(arbiter) = &self.arbiter {
            revoke(arbiter, revoked, &leases(&self.devices));
        }
    }

    fn lease(&self, badge: u64) -> Option<&Lease> {
        self.leases().find(|lease| lease.badge == badge)
    }

    fn leases(&self) -> impl Iterator<Item = &Lease> {
        leases(&self.devices)
    }
}

impl Opener<'_> {
    fn open(&mut self, device: &DeviceId, count: Option<u8>) -> Result<Port, SpmiError> {
        let devices = &mut self.bus.devices;
        let index = devices
            .iter()
            .position(|each| each.peripheral.id() == device)
            .ok_or(SpmiError::NoDevice)?;
        let described = devices.get(index).ok_or(SpmiError::NoDevice)?;
        if described.holder.is_some() {
            return Err(SpmiError::Busy);
        }

        let peripheral = match count {
            Some(count) => described
                .peripheral
                .with_blocks(count, devices.iter().map(|each| &each.peripheral))?,
            None => described.peripheral.clone(),
        };

        let minted = self
            .sessions
            .create_session()
            .map_err(|_| SpmiError::NoSpace)?;

        for ppid in peripheral.ppids() {
            let owner = self
                .bus
                .arbiter
                .as_ref()
                .and_then(|arbiter| arbiter.owner(ppid));

            log_fmt(format_args!(
                "{} channel {ppid:#05x} owner {owner:?}",
                peripheral.id().as_str()
            ));
        }

        if let Some(device) = self.bus.devices.get_mut(index) {
            device.holder = Some(Holder {
                badge: minted.badge.get(),
                peripheral,
                leases: Vec::new(),
            });
        }

        Ok(minted.port)
    }
}

impl spmi::Service<Delivery> for Opener<'_> {
    fn open(
        &mut self,
        _frame: &Delivery,
        device: DeviceId,
    ) -> impl IntoWireValue<Delivery, Read = Result<ipc::wire::Session<spmi_device::Protocol>, SpmiError>>
    {
        // Порт сессии устройства выпускает эта шина, и она же обслуживает на нём `SpmiDevice`.
        Opener::open(self, &device, None).map(kernel_objects::Session::from_handle)
    }

    fn open_blocks(
        &mut self,
        _frame: &Delivery,
        device: DeviceId,
        count: u8,
    ) -> impl IntoWireValue<Delivery, Read = Result<ipc::wire::Session<spmi_device::Protocol>, SpmiError>>
    {
        Opener::open(self, &device, Some(count)).map(kernel_objects::Session::from_handle)
    }
}

impl Held<'_> {
    fn lease(&self, interrupt: Interrupt) -> Option<&Lease> {
        self.holder.leases.iter().find(|lease| {
            lease.interrupt.ppid == interrupt.ppid && lease.interrupt.bit == interrupt.bit
        })
    }

    fn grant(&mut self, interrupt: Interrupt) -> Result<Port, DeviceError> {
        if self.lease(interrupt).is_some() {
            return Err(DeviceError::Busy);
        }

        let signal = Signal::create().map_err(|_| DeviceError::Unavailable)?;
        let minted = self
            .sessions
            .create_session()
            .map_err(|_| DeviceError::Unavailable)?;

        // Порт сессии, которой `arm` отказал, уходит без аренды, и шина пропускает её `Gone`.
        self.arbiter.arm(&interrupt)?;
        self.holder.leases.push(Lease {
            badge: minted.badge.get(),
            interrupt,
            signal,
        });

        Ok(minted.port)
    }
}

impl spmi_device::Service<Delivery> for Held<'_> {
    fn read(&mut self, _frame: &Delivery, address: u16, count: u8) -> Result<u64, DeviceError> {
        let span = self.holder.peripheral.span(address, count)?;
        self.arbiter.read(span.ppid, span.offset, count)
    }

    fn write(
        &mut self,
        _frame: &Delivery,
        address: u16,
        count: u8,
        value: u64,
    ) -> Result<(), DeviceError> {
        let span = self.holder.peripheral.span(address, count)?;
        self.arbiter.write(span.ppid, span.offset, count, value)
    }

    fn update(
        &mut self,
        _frame: &Delivery,
        address: u16,
        mask: u8,
        value: u8,
    ) -> Result<(), DeviceError> {
        // Кадр обслуживается под замком шины, и чтение с записью не перемежаются командами
        // других сессий.
        let span = self.holder.peripheral.span(address, 1)?;
        self.arbiter.update(span.ppid, span.offset, mask, value)
    }

    fn irq(
        &mut self,
        _frame: &Delivery,
        resource: u16,
    ) -> impl IntoWireValue<
        Delivery,
        Read = Result<ipc::wire::Session<spmi_interrupt::Protocol>, DeviceError>,
    > {
        let interrupt = self.holder.peripheral.interrupt(resource)?;
        // Порт сессии прерывания выпускает эта шина, и она же обслуживает на нём
        // `SpmiInterrupt`.
        self.grant(interrupt)
            .map(kernel_objects::Session::from_handle)
    }

    fn block_irq(
        &mut self,
        _frame: &Delivery,
        block: u8,
        bit: u8,
        trigger: Trigger,
    ) -> impl IntoWireValue<
        Delivery,
        Read = Result<ipc::wire::Session<spmi_interrupt::Protocol>, DeviceError>,
    > {
        let interrupt = self
            .holder
            .peripheral
            .block_interrupt(block, bit, trigger)?;
        self.grant(interrupt)
            .map(kernel_objects::Session::from_handle)
    }
}

impl spmi_interrupt::Service<Delivery> for InterruptSession<'_> {
    fn signal(
        &mut self,
        _frame: &Delivery,
    ) -> impl IntoWireValue<Delivery, Read = Result<ipc::wire::Cap<syscall::kind::Signal>, DeviceError>>
    {
        let (_, lease) = self.0.ok_or(DeviceError::Unavailable)?;
        lease
            .signal
            .duplicate(SIGNAL_RIGHTS, None)
            .map_err(|_| DeviceError::Unavailable)
    }

    fn ack(&mut self, _frame: &Delivery) -> Result<(), DeviceError> {
        let (arbiter, lease) = self.0.ok_or(DeviceError::Unavailable)?;

        lease
            .signal
            .set(0, SIGNALED, WakeCount::None)
            .map_err(|_| DeviceError::Unavailable)?;

        arbiter.rearm(&lease.interrupt)
    }
}

fn revoke<'l>(
    arbiter: &Arbiter<Windows>,
    revoked: &[Lease],
    remaining: &(impl Iterator<Item = &'l Lease> + Clone),
) {
    for lease in revoked {
        let _ = arbiter.mask(lease.interrupt.ppid, 1 << lease.interrupt.bit);
    }

    for lease in revoked {
        let shared = remaining
            .clone()
            .any(|other| other.interrupt.ppid == lease.interrupt.ppid);

        if !shared {
            arbiter.disable(lease.interrupt.ppid);
        }
    }
}

fn leases(devices: &[Device]) -> impl Iterator<Item = &Lease> + Clone {
    devices
        .iter()
        .filter_map(|device| device.holder.as_ref())
        .flat_map(|holder| holder.leases.iter())
}

pub(crate) fn serve(bus: &Mutex<Bus>, sessions: &SessionPort) {
    let mut events = sessions.events();

    while let Some(event) = events.recv() {
        match event {
            SessionEvent::Frame(frame) => {
                let badge = frame.resources.badge;
                let mut bus = bus.lock();

                let lease = bus.arbiter.as_ref().zip(bus.lease(badge));
                if lease.is_some() || bus.departed_interrupts.contains(&badge) {
                    let _ = spmi_interrupt::dispatch(&mut InterruptSession(lease), frame);
                    continue;
                }

                let Bus {
                    arbiter, devices, ..
                } = &mut *bus;
                let holder = devices
                    .iter_mut()
                    .filter_map(|device| device.holder.as_mut())
                    .find(|holder| holder.badge == badge);

                if let Some((arbiter, holder)) = arbiter.as_ref().zip(holder) {
                    let mut held = Held {
                        sessions,
                        arbiter,
                        holder,
                    };
                    let _ = spmi_device::dispatch(&mut held, frame);
                } else {
                    let mut opener = Opener {
                        sessions,
                        bus: &mut bus,
                    };
                    let _ = spmi::dispatch(&mut opener, frame);
                }
            }
            SessionEvent::Gone(badge) => bus.lock().release(badge),
        }
    }
}
