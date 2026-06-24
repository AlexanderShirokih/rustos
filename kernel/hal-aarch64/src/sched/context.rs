use core::{mem, ptr::NonNull};

use memory::memory_mapper::AddressSpaceHandle;
use scheduler::{ArchContext, UserEntry};

use super::{
    cpu_local::Aarch64Cpu,
    stack::Aarch64Stack,
    switch::{context_start, context_switch},
};
use crate::{exception::gpreg::GpReg, memory::asid::unpack_asid};

/// Callee-saved состояние потока согласно AAPCS64:
/// - x19..x28 (10 GPR)
/// - x29 (FP), x30 (LR)
/// - SP
/// - NZCV
/// - DAIF (маска IRQ/preemption)
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
    pub nzcv: u64,
    pub daif: u64,
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
/// Offset до nzcv.
pub const CTX_OFFSET_NZCV: usize = CTX_OFFSET_SP + 8;
/// Offset до daif.
pub const CTX_OFFSET_DAIF: usize = CTX_OFFSET_NZCV + 8;
/// Offset до d8.
pub const CTX_OFFSET_D8: usize = CTX_OFFSET_DAIF + 8;

const _: () = assert!(CTX_OFFSET_X19_X28 == core::mem::offset_of!(Aarch64Context, x19_x28));
const _: () = assert!(CTX_OFFSET_FP == core::mem::offset_of!(Aarch64Context, fp));
const _: () = assert!(CTX_OFFSET_LR == core::mem::offset_of!(Aarch64Context, lr));
const _: () = assert!(CTX_OFFSET_SP == core::mem::offset_of!(Aarch64Context, sp));
const _: () = assert!(CTX_OFFSET_NZCV == core::mem::offset_of!(Aarch64Context, nzcv));
const _: () = assert!(CTX_OFFSET_DAIF == core::mem::offset_of!(Aarch64Context, daif));
const _: () = assert!(CTX_OFFSET_D8 == core::mem::offset_of!(Aarch64Context, d8_d15));
const _: () = assert!(mem::size_of::<Aarch64Context>() == CTX_OFFSET_D8 + 8 * 8);

impl ArchContext for Aarch64Context {
    type Cpu = Aarch64Cpu;
    type Stack = Aarch64Stack;

    // Полный нижний 48-бит TTBR0; старший валидный user-байт 0x0000_FFFF_FFFF_FFFF.
    const USER_VA_END: usize = 0x0001_0000_0000_0000;

    fn init(stack_top: NonNull<u8>, entry: scheduler::arch::TrampolineFn, arg: *mut ()) -> Self {
        let sp = (stack_top.as_ptr() as usize & !0xF) as u64;
        let mut context = Self {
            x19_x28: [GpReg::from_u64(0); 10],
            fp: GpReg::from_u64(0),
            lr: GpReg::from_u64(thread_entry_shim as *const () as usize as u64),
            sp,
            nzcv: 0,
            daif: 0,
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

    fn switch_address_space(next: Option<AddressSpaceHandle>) {
        let raw_ttbr: u64 = match next {
            None => 0,
            Some(handle) => {
                let asid = unpack_asid(handle.tag.raw());
                (u64::from(asid) << 48) | (handle.root.as_usize() as u64)
            }
        };
        // SAFETY: TTBR0_EL1 пишется на EL1; `raw_ttbr` - 0 либо PA живого
        // L0-root, объединённый с ASID валидного AS. Запись + isb достаточна:
        // ASID-теги изолируют записи прошлого AS, full-flush не требуется.
        unsafe {
            core::arch::asm!(
                "msr ttbr0_el1, {root}",
                "isb",
                root = in(reg) raw_ttbr,
                options(nostack, preserves_flags),
            );
        }
    }

    fn init_user(entry: UserEntry) -> Self {
        let kernel_sp = (entry.kernel_stack_top.as_ptr() as usize & !0xF) as u64;
        debug_assert_eq!(
            entry.user_sp.as_usize() & 0xF,
            0,
            "user_sp must be 16-byte aligned"
        );

        let mut ctx = Self {
            x19_x28: [GpReg::from_u64(0); 10],
            fp: GpReg::from_u64(0),
            lr: GpReg::from_u64(el0_entry_shim as *const () as usize as u64),
            sp: kernel_sp,
            nzcv: 0,
            daif: 0,
            d8_d15: [0; 8],
        };
        ctx.x19_x28[0] = GpReg::from_u64(entry.arg.0);
        ctx.x19_x28[1] = GpReg::from_u64(entry.user_pc.as_usize() as u64);
        ctx.x19_x28[2] = GpReg::from_u64(entry.user_sp.as_usize() as u64);
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
/// `NZCV=0`.
///
/// Перед `eret` обнуляем все остальные GPR и `tpidr_el0`, чтобы kernel-значения
/// не утекали в EL0 через регистры.
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
