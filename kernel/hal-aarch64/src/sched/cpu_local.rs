use scheduler::{ArchCpu, CpuId};

use crate::{cpu, read_sysreg};

pub struct Aarch64Cpu;

impl ArchCpu for Aarch64Cpu {
    fn current_id() -> CpuId {
        cpu::current_id()
    }

    unsafe fn install_cpu_local(scheduler_cpu: *mut ()) {
        // Per-CPU блок платформенного слоя установлен раньше: его ставит
        // boot-путь CPU, а планировщик лишь добавляет своё состояние.
        let Some(local) = cpu::current() else {
            debug_assert!(
                false,
                "CpuLocal must be installed before scheduler bootstrap"
            );
            return;
        };
        local.set_scheduler(scheduler_cpu);
    }

    fn cpu_local_ptr() -> *mut () {
        cpu::current().map_or(core::ptr::null_mut(), cpu::CpuLocal::scheduler)
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

    fn preemption_enabled() -> bool {
        // SAFETY: DAIF читается на EL1 без побочных эффектов.
        let daif = unsafe { read_sysreg!(daif) };
        // Бит I (IRQ mask) - позиция 7 в DAIF; 0 означает, что IRQ разрешены.
        daif & (1 << 7) == 0
    }
}
