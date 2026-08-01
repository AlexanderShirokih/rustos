//! Точка входа вторичного CPU и его путь до idle.
//!
//! CPU входит сюда по физическому адресу с выключенными MMU и кэшами. До
//! включения MMU действуют те же ограничения, что и в `boot_early`: адреса
//! физические, `klog` недоступен, спинлоки неприменимы.

use core::arch::naked_asm;

use klog::{info, warn};
use memory::physical_address::PhysicalAddress;
use scheduler::ArchCpu;

use super::handoff::{self, HANDOFF, SECONDARY_STACK_SIZE, SECONDARY_STACKS};
use crate::{
    consts::HIGHER_HALF_BASE,
    cpu,
    exception::ExceptionVectors,
    memory::{
        mmu::{Mmu, NormalDualSpaceConfig},
        regs::common::EL1,
    },
    sched::Aarch64Cpu,
};

/// Физический адрес точки входа: его boot CPU передаёт в `CPU_ON` и пишет по
/// `cpu-release-addr`.
pub(super) fn entry_point_physical() -> usize {
    secondary_start as *const () as usize - HIGHER_HALF_BASE
}

// Точка входа вторичного CPU. Регистры при входе не несут параметров:
// spin-table их не передаёт, а `context_id` из PSCI не используется.
// Идентификатор читается из блока передачи параметров.
#[unsafe(naked)]
unsafe extern "C" fn secondary_start() {
    naked_asm!(
        "mrs    x0, CurrentEL",
        "cmp    x0, #0x8",              // EL2?
        "b.ne   1f",

        // HCR_EL2: EL1 в AArch64
        "mov    x0, #(1 << 31)",
        "msr    hcr_el2, x0",

        // Отключение ловушек SIMD/FP
        "mov    x0, #0x33ff",
        "msr    cptr_el2, x0",

        // Разрешаем EL1 доступ к физическому таймеру/счётчику
        "mrs    x0, cnthctl_el2",
        "orr    x0, x0, #0b11",
        "msr    cnthctl_el2, x0",
        "msr    cntvoff_el2, xzr",

        // SPSR_EL2: возврат в EL1h, DAIF masked
        "mov    x0, #0x3c5",
        "msr    spsr_el2, x0",
        "adr    x0, 1f",
        "msr    elr_el2, x0",
        "eret",

        // EL1
        "1:",
        "msr    spsel, #1",

        // Per-CPU блок ещё не установлен: после reset TPIDR_EL1 UNKNOWN.
        "msr    tpidr_el1, xzr",

        // Включение FP/SIMD
        "mrs    x0, cpacr_el1",
        "orr    x0, x0, #(0x3 << 20)",
        "msr    cpacr_el1, x0",
        "isb",

        // Первые два поля блока передачи: логический идентификатор и номер
        // загрузочного стека.
        "adrp   x19, {handoff}",
        "add    x19, x19, #:lo12:{handoff}",
        "ldr    x22, [x19, #8]",
        "ldr    x19, [x19]",

        // Границы своего стека: дно = база + номер * размер, вершина = дно + размер.
        "adrp   x20, {stacks}",
        "add    x20, x20, #:lo12:{stacks}",
        "mov    x21, #{stack_size}",
        "madd   x20, x22, x21, x20",
        "add    x21, x20, x21",

        // Сброс строк кэша над своим стеком: записи ниже идут при выключенном
        // MMU, мимо кэша, и вытеснение чужой строки ложится поверх них.
        "mrs    x2, ctr_el0",
        "ubfx   x2, x2, #16, #4",       // CTR_EL0.DminLine
        "mov    x3, #4",
        "lsl    x2, x3, x2",            // длина строки в байтах
        "mov    x0, x20",
        "2:",
        "dc     ivac, x0",
        "add    x0, x0, x2",
        "cmp    x0, x21",
        "b.lo   2b",
        "dsb    sy",

        "mov    sp, x21",
        "mov    x0, x19",
        "b      {secondary_boot}",

        handoff        = sym HANDOFF,
        stacks         = sym SECONDARY_STACKS,
        stack_size     = const SECONDARY_STACK_SIZE,
        secondary_boot = sym secondary_boot,
    )
}

// Pre-MMU фаза: включение MMU теми же корнями, что у boot CPU, и уход в
// higher half.
extern "C" fn secondary_boot(cpu_id: u64) -> ! {
    let roots = handoff::roots();

    Mmu::<EL1>::enable(&NormalDualSpaceConfig::new(
        PhysicalAddress::new(roots.lower as usize),
        PhysicalAddress::new(roots.higher as usize),
    ));

    // SAFETY: MMU включён, TTBR1 содержит тот же higher-half, по которому
    // работает boot CPU; identity-отображение TTBR0 покрывает текущий PC.
    unsafe { jump_to_higher_half(cpu_id) }
}

/// # Safety
///
/// MMU включён, TTBR1 содержит higher-half, TTBR0 покрывает текущий PC.
#[unsafe(naked)]
unsafe extern "C" fn jump_to_higher_half(_cpu_id: u64) -> ! {
    const HALF_47_32: u64 = ((HIGHER_HALF_BASE as u64) >> 32) & 0xFFFF;
    const HALF_63_48: u64 = ((HIGHER_HALF_BASE as u64) >> 48) & 0xFFFF;

    naked_asm!(
        "mov    x19, x0",

        "movz   x1, #{half_32}, lsl #32",
        "movk   x1, #{half_48}, lsl #48",

        // PC в higher half.
        "adr    x0, 1f",
        "add    x0, x0, x1",
        "br     x0",

        "1:",
        // Стек - тот же, но по виртуальному адресу.
        "mov    x2, sp",
        "add    x2, x2, x1",
        "mov    sp, x2",

        "mov    x0, x19",
        "b      {secondary_main}",

        half_32        = const HALF_47_32,
        half_48        = const HALF_63_48,
        secondary_main = sym secondary_main,
    )
}

extern "C" fn secondary_main(cpu_id: u64) -> ! {
    Mmu::<EL1>::disable_lower_half();

    ExceptionVectors::instance().install();

    // Идентификатор берётся из таблицы топологии по собственному affinity и
    // сверяется со слотом: расхождение значит, что слот уже переиспользован под
    // другой CPU. Ниже по всем отказам CPU уходит в idle, не доложив о
    // готовности, и boot CPU прекращает подъём.
    let Some(id) = cpu::confirm_current_id(cpu_id) else {
        warn!("CPU with handoff id {cpu_id} does not match the topology, parking it");
        Aarch64Cpu::idle();
    };

    // CPU, не получивший ни блока, ни своего интерфейса контроллера
    // прерываний, в работу не идёт и о готовности не докладывает: boot CPU
    // увидит отказ и остановит подъём.
    if cpu::install_local(id).is_none() {
        warn!("CPU {} has no per-CPU block for this id", id.raw());
        Aarch64Cpu::idle();
    }

    if let Err(error) = super::per_cpu_init() {
        warn!("CPU {}: {error}", id.raw());
        Aarch64Cpu::idle();
    }

    // Per-CPU состояние планировщика поднимается до отметки о готовности:
    // отказ здесь ещё возвращается, а после отметки boot CPU уже считает CPU
    // рабочим.
    if let Err(error) = kernelspace::sched_bridge::bootstrap_this_cpu() {
        warn!("CPU {}: {error}", id.raw());
        Aarch64Cpu::idle();
    }

    info!("CPU {} is online", id.raw());

    // Отметка перед входом в планировщик: вход не возвращается, а слот
    // параметров нужно освободить под следующий CPU.
    if !handoff::mark_online() {
        warn!(
            "CPU {} reported readiness after bring-up was closed",
            id.raw()
        );
        Aarch64Cpu::idle();
    }

    // Вход не возвращается: CPU уходит в свой idle-поток и дальше живёт под
    // управлением планировщика, размаскировав IRQ.
    let error = kernelspace::sched_bridge::enter_scheduler();
    warn!("CPU {} did not enter the scheduler: {error}", id.raw());

    Aarch64Cpu::idle()
}
