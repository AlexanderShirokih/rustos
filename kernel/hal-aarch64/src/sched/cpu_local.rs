use drivers_common_aarch64::ipi_affinity::IpiAffinity;
use klog::warn;
use scheduler::{ArchCpu, CpuId, CpuLocalError};

use crate::{cpu, read_sysreg};

pub struct Aarch64Cpu;

impl ArchCpu for Aarch64Cpu {
    fn current_id() -> CpuId {
        cpu::current_id()
    }

    unsafe fn install_cpu_local(scheduler_cpu: *mut ()) -> Result<(), CpuLocalError> {
        // Per-CPU блок платформенного слоя установлен раньше. Его ставит boot-путь
        // CPU, а планировщик лишь добавляет своё состояние.
        let local = cpu::current().ok_or(CpuLocalError::NotInstalled)?;
        local.set_scheduler(scheduler_cpu);
        Ok(())
    }

    fn cpu_local_ptr() -> *mut () {
        cpu::current().map_or(core::ptr::null_mut(), cpu::CpuLocal::scheduler)
    }

    fn start_secondaries() {
        crate::smp::start_secondaries();
    }

    fn send_reschedule(target: CpuId) {
        // Контроллер адресует CPU по affinity, а не по номеру ядра. Перевод
        // делает таблица топологии.
        let Some(affinity) = cpu::affinity(target.as_index()) else {
            warn!(
                "CPU {} is not in the topology, reschedule was not sent",
                target.raw()
            );
            return;
        };

        kernelspace::sched_bridge::send_reschedule(
            IpiAffinity::from_mpidr(affinity.raw()).to_target(),
        );
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
