//! Сессии `Spmi`, сессии устройств и сессии прерываний выпускаются на одном порту из общего
//! счётчика значков. Кадр значка, которым занято устройство, обслуживает протокол `SpmiDevice`,
//! кадр значка аренды прерывания либо сессии прерывания ушедшего устройства - протокол
//! `SpmiInterrupt`, остальные кадры - протокол `Spmi`.

use alloc::{vec, vec::Vec};
use core::cell::{Ref, RefCell, RefMut};

use component_startup::SessionPort;
use ipc::{Incoming, IntoWireValue, wire::IpcError};
use kernel_objects::{Delivery, Port, Signal};
use platform_abi::DeviceId;
use runtime::log_fmt;
use spmi_abi::{DeviceError, SpmiError, Trigger, spmi, spmi_device, spmi_interrupt};
use spmi_pmic_arb::{Arbiter, Interrupt, Peripheral};
use syscall::{Rights, SIGNALED, WakeCount};

use crate::windows::Windows;

const SIGNAL_RIGHTS: Rights = Rights::READ
    .union(Rights::DUPLICATE)
    .union(Rights::TRANSFER);

pub(crate) struct Bus {
    pub(crate) arbiter: Option<Arbiter<Windows>>,
    pub(crate) devices: RefCell<Vec<Device>>,
    pub(crate) departed_interrupts: RefCell<Vec<u64>>,
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
    bus: &'a Bus,
}

struct Held<'a> {
    sessions: &'a SessionPort,
    bus: &'a Bus,
    badge: u64,
}

struct InterruptSession<'a> {
    bus: &'a Bus,
    badge: u64,
}

impl Bus {
    pub(crate) fn deliver(&self) {
        let Some(arbiter) = &self.arbiter else {
            return;
        };

        arbiter.drain(|ppid, bits| {
            let devices = self.devices.borrow();
            let fired = leases(&devices).filter(|lease| {
                lease.interrupt.ppid == ppid && bits & (1 << lease.interrupt.bit) != 0
            });
            for lease in fired {
                let _ = lease.signal.notify_all();
            }
        });
    }

    pub(crate) fn release(&self, badge: u64) {
        let revoked = {
            let mut devices = self.devices.borrow_mut();
            if let Some(holder) = devices
                .iter_mut()
                .find_map(|device| device.holder.take_if(|holder| holder.badge == badge))
            {
                self.departed_interrupts
                    .borrow_mut()
                    .extend(holder.leases.iter().map(|lease| lease.badge));
                holder.leases
            } else if let Some(lease) = devices.iter_mut().find_map(|device| {
                let leases = &mut device.holder.as_mut()?.leases;
                let index = leases.iter().position(|lease| lease.badge == badge)?;
                Some(leases.remove(index))
            }) {
                vec![lease]
            } else {
                self.departed_interrupts
                    .borrow_mut()
                    .retain(|departed| *departed != badge);
                Vec::new()
            }
        };

        if let Some(arbiter) = &self.arbiter {
            revoke(arbiter, &revoked, &leases(&self.devices.borrow()));
        }
    }

    fn lease(&self, badge: u64) -> Option<Ref<'_, Lease>> {
        Ref::filter_map(self.devices.borrow(), |devices| {
            leases(devices).find(|lease| lease.badge == badge)
        })
        .ok()
    }

    pub(crate) fn dispatch(
        &self,
        sessions: &SessionPort,
        frame: Incoming<'_, Delivery>,
    ) -> Result<(), IpcError> {
        let badge = frame.resources.badge;
        let interrupt =
            self.lease(badge).is_some() || self.departed_interrupts.borrow().contains(&badge);
        if interrupt {
            return spmi_interrupt::dispatch(&InterruptSession { bus: self, badge }, frame);
        }

        let held = self.devices.borrow().iter().any(|device| {
            device
                .holder
                .as_ref()
                .is_some_and(|holder| holder.badge == badge)
        });
        if held && self.arbiter.is_some() {
            spmi_device::dispatch(
                &Held {
                    sessions,
                    bus: self,
                    badge,
                },
                frame,
            )
        } else {
            spmi::dispatch(
                &Opener {
                    sessions,
                    bus: self,
                },
                frame,
            )
        }
    }
}

impl Opener<'_> {
    fn open(&self, device: &DeviceId, count: Option<u8>) -> Result<Port, SpmiError> {
        let mut devices = self.bus.devices.borrow_mut();
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

        if let Some(device) = devices.get_mut(index) {
            device.holder = Some(Holder {
                badge: minted.badge.get(),
                peripheral,
                leases: Vec::new(),
            });
        }

        Ok(minted.port)
    }
}

#[allow(clippy::unused_async_trait_impl)]
impl spmi::Service<Delivery> for Opener<'_> {
    fn open(
        &self,
        _frame: &Delivery,
        device: DeviceId,
    ) -> impl IntoWireValue<Delivery, Read = Result<ipc::wire::Session<spmi_device::Protocol>, SpmiError>>
    {
        // Порт сессии устройства выпускает эта шина, и она же обслуживает на нём `SpmiDevice`.
        Opener::open(self, &device, None).map(kernel_objects::Session::from_handle)
    }

    fn open_blocks(
        &self,
        _frame: &Delivery,
        device: DeviceId,
        count: u8,
    ) -> impl IntoWireValue<Delivery, Read = Result<ipc::wire::Session<spmi_device::Protocol>, SpmiError>>
    {
        Opener::open(self, &device, Some(count)).map(kernel_objects::Session::from_handle)
    }
}

impl Held<'_> {
    fn holder(&self) -> Result<RefMut<'_, Holder>, DeviceError> {
        RefMut::filter_map(self.bus.devices.borrow_mut(), |devices| {
            devices
                .iter_mut()
                .filter_map(|device| device.holder.as_mut())
                .find(|holder| holder.badge == self.badge)
        })
        .map_err(|_| DeviceError::Unavailable)
    }

    fn arbiter(&self) -> Result<&Arbiter<Windows>, DeviceError> {
        self.bus.arbiter.as_ref().ok_or(DeviceError::Unavailable)
    }

    fn grant(&self, interrupt: Interrupt) -> Result<Port, DeviceError> {
        let mut holder = self.holder()?;
        if holder.leases.iter().any(|lease| {
            lease.interrupt.ppid == interrupt.ppid && lease.interrupt.bit == interrupt.bit
        }) {
            return Err(DeviceError::Busy);
        }

        let signal = Signal::create().map_err(|_| DeviceError::Unavailable)?;
        let minted = self
            .sessions
            .create_session()
            .map_err(|_| DeviceError::Unavailable)?;
        self.arbiter()?.arm(&interrupt)?;
        holder.leases.push(Lease {
            badge: minted.badge.get(),
            interrupt,
            signal,
        });
        Ok(minted.port)
    }
}

#[allow(clippy::unused_async_trait_impl)]
impl spmi_device::Service<Delivery> for Held<'_> {
    fn read(&self, _frame: &Delivery, address: u16, count: u8) -> Result<u64, DeviceError> {
        let span = self.holder()?.peripheral.span(address, count)?;
        self.arbiter()?.read(span.ppid, span.offset, count)
    }

    fn write(
        &self,
        _frame: &Delivery,
        address: u16,
        count: u8,
        value: u64,
    ) -> Result<(), DeviceError> {
        let span = self.holder()?.peripheral.span(address, count)?;
        self.arbiter()?.write(span.ppid, span.offset, count, value)
    }

    fn update(
        &self,
        _frame: &Delivery,
        address: u16,
        mask: u8,
        value: u8,
    ) -> Result<(), DeviceError> {
        // Чтение с записью выполняются в одном poll без уступки другим задачам.
        let span = self.holder()?.peripheral.span(address, 1)?;
        self.arbiter()?.update(span.ppid, span.offset, mask, value)
    }

    fn irq(
        &self,
        _frame: &Delivery,
        resource: u16,
    ) -> impl IntoWireValue<
        Delivery,
        Read = Result<ipc::wire::Session<spmi_interrupt::Protocol>, DeviceError>,
    > {
        let interrupt = self.holder()?.peripheral.interrupt(resource)?;
        // Порт сессии прерывания выпускает эта шина, и она же обслуживает на нём
        // `SpmiInterrupt`.
        self.grant(interrupt)
            .map(kernel_objects::Session::from_handle)
    }

    fn block_irq(
        &self,
        _frame: &Delivery,
        block: u8,
        bit: u8,
        trigger: Trigger,
    ) -> impl IntoWireValue<
        Delivery,
        Read = Result<ipc::wire::Session<spmi_interrupt::Protocol>, DeviceError>,
    > {
        let interrupt = self
            .holder()?
            .peripheral
            .block_interrupt(block, bit, trigger)?;
        self.grant(interrupt)
            .map(kernel_objects::Session::from_handle)
    }
}

#[allow(clippy::unused_async_trait_impl)]
impl spmi_interrupt::Service<Delivery> for InterruptSession<'_> {
    fn signal(
        &self,
        _frame: &Delivery,
    ) -> impl IntoWireValue<Delivery, Read = Result<ipc::wire::Cap<syscall::kind::Signal>, DeviceError>>
    {
        let lease = self.bus.lease(self.badge).ok_or(DeviceError::Unavailable)?;
        lease
            .signal
            .duplicate(SIGNAL_RIGHTS, None)
            .map_err(|_| DeviceError::Unavailable)
    }

    fn ack(&self, _frame: &Delivery) -> Result<(), DeviceError> {
        let arbiter = self.bus.arbiter.as_ref().ok_or(DeviceError::Unavailable)?;
        let lease = self.bus.lease(self.badge).ok_or(DeviceError::Unavailable)?;

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
