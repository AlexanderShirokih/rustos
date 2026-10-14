//! Регистры TLMM и прерывания выводов, выданные сессиям `DevicePins`, по номерам выводов.

use alloc::collections::BTreeMap;

use kernel_objects::Signal;
use pinctrl_msm::{Line, Tlmm};
use pins_abi::PinsError;
use syscall::{Rights, SIGNALED, WakeCount};

use crate::windows::Windows;

const SIGNAL_RIGHTS: Rights = Rights::READ
    .union(Rights::DUPLICATE)
    .union(Rights::TRANSFER);

pub(crate) struct Hardware {
    pub(crate) tlmm: Tlmm<Windows>,
    irqs: BTreeMap<u16, Irq>,
}

struct Irq {
    badge: u64,
    number: u8,
    line: Line,
    signal: Signal,
}

impl Hardware {
    pub(crate) const fn new(tlmm: Tlmm<Windows>) -> Self {
        Self {
            tlmm,
            irqs: BTreeMap::new(),
        }
    }

    pub(crate) fn irq(&mut self, badge: u64, number: u8, line: &Line) -> Result<Signal, PinsError> {
        if let Some(irq) = self.irqs.get(&line.pin.number) {
            if irq.badge != badge || irq.number != number {
                return Err(PinsError::Busy);
            }
            return duplicate(&irq.signal);
        }

        let signal = Signal::create().map_err(|_| PinsError::NoSpace)?;
        let copy = duplicate(&signal)?;
        self.tlmm.enable_irq(line);
        self.irqs.insert(
            line.pin.number,
            Irq {
                badge,
                number,
                line: line.clone(),
                signal,
            },
        );
        Ok(copy)
    }

    pub(crate) fn ack(&self, badge: u64, number: u8, line: &Line) -> Result<(), PinsError> {
        let irq = self
            .irqs
            .get(&line.pin.number)
            .filter(|irq| irq.badge == badge && irq.number == number)
            .ok_or(PinsError::NotIssued)?;

        irq.signal
            .set(0, SIGNALED, WakeCount::None)
            .map_err(|_| PinsError::Failed)?;
        self.tlmm.ack_irq(&irq.line);
        Ok(())
    }

    pub(crate) fn release(&mut self, badge: u64) {
        let tlmm = &self.tlmm;
        self.irqs.retain(|_, irq| {
            let kept = irq.badge != badge;
            if !kept {
                tlmm.disable_irq(&irq.line);
            }
            kept
        });
    }

    pub(crate) fn deliver(&self) -> bool {
        let mut delivered = false;
        for irq in self.irqs.values() {
            if self.tlmm.irq_fired(&irq.line) {
                self.tlmm.hold_irq(&irq.line);
                let _ = irq.signal.notify_all();
                delivered = true;
            }
        }
        delivered
    }
}

fn duplicate(signal: &Signal) -> Result<Signal, PinsError> {
    signal
        .duplicate(SIGNAL_RIGHTS, None)
        .map_err(|_| PinsError::NoSpace)
}
