//! Вызов функций PSCI по соглашению SMCCC.

use hal_common::{cpu::CpuAffinity, smp::PsciConduit};

const PSCI_SYSTEM_OFF: u64 = 0x8400_0008;

#[derive(Clone, Copy)]
pub(crate) enum PsciCall {
    CpuOn {
        function: u32,
        target: CpuAffinity,
        entry_point: u64,
    },
    SystemOff,
}

impl PsciCall {
    fn registers(self) -> [u64; 4] {
        match self {
            PsciCall::CpuOn {
                function,
                target,
                entry_point,
            } => [u64::from(function), target.raw(), entry_point, 0],
            PsciCall::SystemOff => [PSCI_SYSTEM_OFF, 0, 0, 0],
        }
    }
}

// SAFETY: вызов уходит на уровень выше EL1 и обязан быть корректным вызовом PSCI.
pub(crate) unsafe fn psci_call(conduit: PsciConduit, call: PsciCall) -> u64 {
    let [function, arg0, arg1, arg2] = call.registers();
    let result: u64;

    match conduit {
        PsciConduit::Hvc => {
            // SAFETY: `hvc #0` - вызов гипервизора по SMCCC: аргументы в x0-x3,
            // результат в x0. Регистры x4-x17 соглашение объявляет затираемыми.
            unsafe {
                core::arch::asm!(
                    "hvc #0",
                    inlateout("x0") function => result,
                    inlateout("x1") arg0 => _,
                    inlateout("x2") arg1 => _,
                    inlateout("x3") arg2 => _,
                    lateout("x4") _, lateout("x5") _, lateout("x6") _, lateout("x7") _,
                    lateout("x8") _, lateout("x9") _, lateout("x10") _, lateout("x11") _,
                    lateout("x12") _, lateout("x13") _, lateout("x14") _, lateout("x15") _,
                    lateout("x16") _, lateout("x17") _,
                    options(nostack),
                );
            }
        }
        PsciConduit::Smc => {
            // SAFETY: см. ветку `hvc`; `smc #0` отличается только уровнем,
            // на который уходит вызов.
            unsafe {
                core::arch::asm!(
                    "smc #0",
                    inlateout("x0") function => result,
                    inlateout("x1") arg0 => _,
                    inlateout("x2") arg1 => _,
                    inlateout("x3") arg2 => _,
                    lateout("x4") _, lateout("x5") _, lateout("x6") _, lateout("x7") _,
                    lateout("x8") _, lateout("x9") _, lateout("x10") _, lateout("x11") _,
                    lateout("x12") _, lateout("x13") _, lateout("x14") _, lateout("x15") _,
                    lateout("x16") _, lateout("x17") _,
                    options(nostack),
                );
            }
        }
    }

    result
}
