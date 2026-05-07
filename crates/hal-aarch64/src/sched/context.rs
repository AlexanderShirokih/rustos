use core::{mem, ptr::NonNull};

use main::sched::ArchContext;

use super::{
    cpu_local::Aarch64Cpu,
    stack::Aarch64Stack,
    switch::{context_start, context_switch},
};
use crate::exception::gpreg::GpReg;

/// Callee-saved состояние потока согласно AAPCS64:
/// - x19..x28 (10 GPR)
/// - x29 (FP), x30 (LR)
/// - SP
/// - NZCV (через PSTATE-snapshot)
/// - d8..d15 (нижние 64 бит callee-saved SIMD/FP регистров)
///
/// Структура `repr(C)`, поля упорядочены строго по offset-ам, на которые
/// ссылается ассемблер в `switch.rs`. Любое изменение требует синхронизации.
///
/// `GpReg` - `#[repr(transparent)] u64` обёртка для семантической типизации
/// регистров общего назначения (x19..x30). Asm читает их как `u64` через
/// фиксированные offsets - layout не меняется.
#[repr(C)]
pub struct Aarch64Context {
    pub x19_x28: [GpReg; 10],
    pub fp: GpReg,
    pub lr: GpReg,
    pub sp: u64,
    pub pstate: u64,
    pub d8_d15: [u64; 8],
}

/// Offset до x19 = 0.
pub const CTX_OFFSET_X19_X28: usize = 0;
/// Offset до fp.
pub const CTX_OFFSET_FP: usize = CTX_OFFSET_X19_X28 + 8 * 10;
/// Offset до lr.
pub const CTX_OFFSET_LR: usize = CTX_OFFSET_FP + 8;
/// Offset до sp.
pub const CTX_OFFSET_SP: usize = CTX_OFFSET_LR + 8;
/// Offset до pstate.
pub const CTX_OFFSET_PSTATE: usize = CTX_OFFSET_SP + 8;
/// Offset до d8.
pub const CTX_OFFSET_D8: usize = CTX_OFFSET_PSTATE + 8;

const _: () = assert!(CTX_OFFSET_X19_X28 == core::mem::offset_of!(Aarch64Context, x19_x28));
const _: () = assert!(CTX_OFFSET_FP == core::mem::offset_of!(Aarch64Context, fp));
const _: () = assert!(CTX_OFFSET_LR == core::mem::offset_of!(Aarch64Context, lr));
const _: () = assert!(CTX_OFFSET_SP == core::mem::offset_of!(Aarch64Context, sp));
const _: () = assert!(CTX_OFFSET_PSTATE == core::mem::offset_of!(Aarch64Context, pstate));
const _: () = assert!(CTX_OFFSET_D8 == core::mem::offset_of!(Aarch64Context, d8_d15));
const _: () = assert!(mem::size_of::<Aarch64Context>() == CTX_OFFSET_D8 + 8 * 8);

impl ArchContext for Aarch64Context {
    type Cpu = Aarch64Cpu;
    type Stack = Aarch64Stack;

    fn init(stack_top: NonNull<u8>, entry: main::sched::arch::TrampolineFn, arg: *mut ()) -> Self {
        let sp = (stack_top.as_ptr() as usize & !0xF) as u64;
        let mut context = Self {
            x19_x28: [GpReg::from_u64(0); 10],
            fp: GpReg::from_u64(0),
            lr: GpReg::from_u64(thread_entry_shim as *const () as usize as u64),
            sp,
            pstate: 0,
            d8_d15: [0; 8],
        };
        context.x19_x28[0] = GpReg::from_u64(arg as usize as u64);
        context.x19_x28[1] = GpReg::from_u64(entry as usize as u64);
        context
    }

    unsafe fn start(next: &Self) -> ! {
        // SAFETY: используется только для первого входа в поток из boot context.
        unsafe { context_start(core::ptr::from_ref::<Self>(next)) }
    }

    unsafe fn switch(prev: &mut Self, next: &Self) {
        // SAFETY: вызывается scheduler при эксклюзивном владении обоими контекстами.
        unsafe {
            context_switch(
                core::ptr::from_mut::<Self>(prev),
                core::ptr::from_ref::<Self>(next),
            );
        }
    }
}

impl Aarch64Context {
    /// Инициализирует контекст для первого входа в EL0.
    ///
    /// `kernel_stack_top` - вершина EL1-стека потока (для будущих SVC из EL0).
    /// `user_pc` - пользовательский entry point (попадает в `ELR_EL1`).
    /// `user_sp` - пользовательский SP (попадает в `SP_EL0`); должен быть выровнен на 16.
    /// `bootstrap_x0` - значение, передаваемое в user-x0 при первом исполнении.
    ///
    /// При первом `start`/`switch` управление через [`context_start`] попадает на
    /// [`el0_entry_shim`], который устанавливает SP_EL0/ELR_EL1/SPSR_EL1 и делает `eret`.
    /// SP_EL1 после `eret` остаётся равным `kernel_stack_top` - последующие исключения
    /// (включая SVC из EL0) попадают на kernel-стек и идут через текущий диспатчер
    /// `sync_lower_el_a64`.
    // Без `qemu-tests`-фичи метод сейчас не вызывается (scheduler-API для user-thread'ов
    // ещё не подключён). После интеграции `Scheduler::spawn_user` атрибут можно убрать.
    #[cfg_attr(not(feature = "qemu-tests"), allow(dead_code))]
    pub fn init_user(
        kernel_stack_top: NonNull<u8>,
        user_pc: usize,
        user_sp: usize,
        bootstrap_x0: u64,
    ) -> Self {
        let kernel_sp = (kernel_stack_top.as_ptr() as usize & !0xF) as u64;
        debug_assert_eq!(user_sp & 0xF, 0, "user_sp must be 16-byte aligned");

        let mut ctx = Self {
            x19_x28: [GpReg::from_u64(0); 10],
            fp: GpReg::from_u64(0),
            lr: GpReg::from_u64(el0_entry_shim as *const () as usize as u64),
            sp: kernel_sp,
            pstate: 0,
            d8_d15: [0; 8],
        };
        ctx.x19_x28[0] = GpReg::from_u64(bootstrap_x0);
        ctx.x19_x28[1] = GpReg::from_u64(user_pc as u64);
        ctx.x19_x28[2] = GpReg::from_u64(user_sp as u64);
        ctx
    }
}

#[unsafe(naked)]
unsafe extern "C" fn thread_entry_shim() -> ! {
    core::arch::naked_asm!("mov x0, x19", "br x20",)
}

/// Шим первого входа в EL0.
///
/// `context_start` восстанавливает callee-saved регистры и делает `ret` сюда. К этому
/// моменту:
/// - `x19` = `bootstrap_x0` -> user `x0`,
/// - `x20` = `user_pc` -> `ELR_EL1`,
/// - `x21` = `user_sp` -> `SP_EL0`,
/// - текущий `sp` = kernel stack top -> останется в `SP_EL1` после `eret`.
///
/// `SPSR_EL1` ставим в 0: `M[3:0]=0000` (EL0t), `M[4]=0` (AArch64), `DAIF=0`,
/// `NZCV=0`. Если в будущем kernel включит PAN - добавить `msr pan, #1` сюда.
///
/// Перед `eret` обнуляем все остальные GPR и `tpidr_el0`, чтобы kernel-значения
/// не утекали в EL0 через регистры.
// Адрес шима берётся только через `init_user` - пока `init_user` сам не вызывается
// без `qemu-tests`-фичи, шим тоже считается dead_code. Атрибут уйдёт вместе с
// интеграцией `Scheduler::spawn_user`.
#[cfg_attr(not(feature = "qemu-tests"), allow(dead_code))]
#[unsafe(naked)]
unsafe extern "C" fn el0_entry_shim() -> ! {
    core::arch::naked_asm!(
        "mov x0, x19",
        "msr sp_el0, x21",
        "msr elr_el1, x20",
        "msr spsr_el1, xzr",
        "msr tpidr_el0, xzr",
        "mov x1,  xzr",
        "mov x2,  xzr",
        "mov x3,  xzr",
        "mov x4,  xzr",
        "mov x5,  xzr",
        "mov x6,  xzr",
        "mov x7,  xzr",
        "mov x8,  xzr",
        "mov x9,  xzr",
        "mov x10, xzr",
        "mov x11, xzr",
        "mov x12, xzr",
        "mov x13, xzr",
        "mov x14, xzr",
        "mov x15, xzr",
        "mov x16, xzr",
        "mov x17, xzr",
        "mov x18, xzr",
        "mov x19, xzr",
        "mov x20, xzr",
        "mov x21, xzr",
        "mov x22, xzr",
        "mov x23, xzr",
        "mov x24, xzr",
        "mov x25, xzr",
        "mov x26, xzr",
        "mov x27, xzr",
        "mov x28, xzr",
        "mov x29, xzr",
        "mov x30, xzr",
        "eret",
    )
}
