//! Сессии `Spmi` и сессии устройств выпускаются на одном порту из общего счётчика значков.
//! Кадр значка, которым занято устройство, обслуживает протокол `SpmiDevice`, остальные
//! кадры - протокол `Spmi`.

use alloc::vec::Vec;

use component_startup::{SessionEvent, SessionPort};
use ipc::IntoWireValue;
use kernel_objects::{Delivery, OwnedHandle, Port, Signal};
use platform_abi::DeviceId;
use runtime::{Mutex, log_fmt};
use spmi_abi::{DeviceError, SpmiError, Trigger, spmi, spmi_device};
use spmi_pmic_arb::{Arbiter, Interrupt, Peripheral};
use syscall::{Rights, SIGNALED, WakeCount, kind};

use crate::windows::Windows;

const SIGNAL_RIGHTS: Rights = Rights::READ
    .union(Rights::DUPLICATE)
    .union(Rights::TRANSFER);

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
    peripheral: Peripheral,
    leases: Vec<Lease>,
}

struct Lease {
    interrupt: Interrupt,
    signal: Signal,
}

struct Opener<'a> {
    sessions: &'a SessionPort,
    bus: &'a mut Bus,
}

struct Held<'a> {
    arbiter: &'a Arbiter<Windows>,
    holder: &'a mut Holder,
    devices: &'a [Device],
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

        if let Some(arbiter) = &self.arbiter {
            revoke(arbiter, &holder.leases, &leases(&self.devices));
        }
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
    fn position(&self, interrupt: Interrupt) -> Option<usize> {
        self.holder.leases.iter().position(|lease| {
            lease.interrupt.ppid == interrupt.ppid && lease.interrupt.bit == interrupt.bit
        })
    }

    fn lease(&self, interrupt: Interrupt) -> Option<&Lease> {
        self.holder.leases.get(self.position(interrupt)?)
    }

    fn grant(&mut self, interrupt: Interrupt) -> Result<OwnedHandle<kind::Signal>, DeviceError> {
        if let Some(lease) = self.lease(interrupt) {
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
        self.holder.leases.push(Lease { interrupt, signal });

        Ok(copy)
    }

    fn acknowledge(&self, interrupt: Interrupt) -> Result<(), DeviceError> {
        let lease = self.lease(interrupt).ok_or(DeviceError::Unavailable)?;

        lease
            .signal
            .set(0, SIGNALED, WakeCount::None)
            .map_err(|_| DeviceError::Unavailable)?;

        self.arbiter.rearm(&lease.interrupt)
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
    ) -> impl IntoWireValue<Delivery, Read = Result<ipc::wire::Cap<syscall::kind::Signal>, DeviceError>>
    {
        let interrupt = self.holder.peripheral.interrupt(resource)?;
        self.grant(interrupt)
    }

    fn ack(&mut self, _frame: &Delivery, resource: u16) -> Result<(), DeviceError> {
        let interrupt = self.holder.peripheral.interrupt(resource)?;
        self.acknowledge(interrupt)
    }

    fn block_irq(
        &mut self,
        _frame: &Delivery,
        block: u8,
        bit: u8,
        trigger: Trigger,
    ) -> impl IntoWireValue<Delivery, Read = Result<ipc::wire::Cap<syscall::kind::Signal>, DeviceError>>
    {
        let interrupt = self
            .holder
            .peripheral
            .block_interrupt(block, bit, trigger)?;
        self.grant(interrupt)
    }

    fn block_ack(&mut self, _frame: &Delivery, block: u8, bit: u8) -> Result<(), DeviceError> {
        let interrupt = self
            .holder
            .peripheral
            .block_interrupt(block, bit, Trigger::Both)?;
        self.acknowledge(interrupt)
    }

    fn block_release(&mut self, _frame: &Delivery, block: u8, bit: u8) -> Result<(), DeviceError> {
        let interrupt = self
            .holder
            .peripheral
            .block_interrupt(block, bit, Trigger::Both)?;
        let index = self.position(interrupt).ok_or(DeviceError::Unavailable)?;

        // Сброс аренды закрывает её `Signal` и отзывает его копии у клиента.
        let lease = self.holder.leases.remove(index);
        let remaining = leases(self.devices).chain(&self.holder.leases);
        revoke(self.arbiter, core::slice::from_ref(&lease), &remaining);
        Ok(())
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
                let Bus { arbiter, devices } = &mut *bus;

                let index = devices.iter().position(|device| {
                    device
                        .holder
                        .as_ref()
                        .is_some_and(|holder| holder.badge == badge)
                });
                let taken = arbiter.as_ref().zip(index).and_then(|(arbiter, index)| {
                    let holder = devices.get_mut(index)?.holder.take()?;
                    Some((arbiter, index, holder))
                });

                // На время кадра `holder` вынут из `devices`, и в `devices` остаются аренды только
                // других сессий.
                if let Some((arbiter, index, mut holder)) = taken {
                    let mut held = Held {
                        arbiter,
                        holder: &mut holder,
                        devices,
                    };
                    let _ = spmi_device::dispatch(&mut held, frame);

                    if let Some(device) = devices.get_mut(index) {
                        device.holder = Some(holder);
                    }
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
