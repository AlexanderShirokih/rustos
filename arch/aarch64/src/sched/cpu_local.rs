use kernel::sched::{ArchCpu, CpuId};

use crate::{read_sysreg, write_sysreg};

pub struct Aarch64Cpu;

impl ArchCpu for Aarch64Cpu {
    fn current_id() -> CpuId {
        // SAFETY: MPIDR_EL1 доступен на EL1 и только читается.
        let mpidr = unsafe { read_sysreg!(mpidr_el1) };
        CpuId::new((mpidr & 0xFF) as u16)
    }

    unsafe fn install_cpu_local(cpu: *mut ()) {
        // SAFETY: TPIDR_EL1 - kernel-private CPU-local pointer на EL1.
        // Указатель должен жить всё время работы scheduler-а на этом CPU.
        unsafe { write_sysreg!(tpidr_el1, cpu as usize as u64) };
    }

    fn cpu_local_ptr() -> *mut () {
        // SAFETY: TPIDR_EL1 ранее установлен через install_cpu_local; либо равен 0
        // до bootstrap (валидное null-значение, scheduler выполняет fallback).
        let raw = unsafe { read_sysreg!(tpidr_el1) };
        raw as usize as *mut ()
    }

    fn idle() -> ! {
        loop {
            // SAFETY: WFI переводит ядро в idle до следующего interrupt event.
            unsafe {
                core::arch::asm!("wfi", options(nomem, nostack, preserves_flags));
            }
        }
    }

    fn enable_preemption() {
        // SAFETY: Разрешаем IRQ, не меняя другие DAIF-биты.
        unsafe {
            core::arch::asm!("msr daifclr, #0b0010", options(nostack, preserves_flags));
        }
    }

    fn disable_preemption() {
        // SAFETY: Маскируем IRQ во время критических участков scheduler.
        unsafe {
            core::arch::asm!("msr daifset, #0b0010", options(nostack, preserves_flags));
        }
    }
}
