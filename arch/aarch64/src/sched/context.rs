use core::{mem, ptr::NonNull};

use kernel::sched::ArchContext;

use super::switch::context_start;
use super::{cpu_local::Aarch64Cpu, stack::Aarch64Stack, switch::context_switch};

/// Callee-saved состояние потока согласно AAPCS64:
/// - x19..x28 (10 GPR)
/// - x29 (FP), x30 (LR)
/// - SP
/// - NZCV (через PSTATE-snapshot)
/// - d8..d15 (нижние 64 бит callee-saved SIMD/FP регистров)
///
/// Структура `repr(C)`, поля упорядочены строго по offset-ам, на которые
/// ссылается ассемблер в `switch.rs`. Любое изменение требует синхронизации.
#[repr(C)]
pub struct Aarch64Context {
    pub x19_x28: [u64; 10],
    pub fp: u64,
    pub lr: u64,
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

    fn init(
        stack_top: NonNull<u8>,
        entry: kernel::sched::arch::TrampolineFn,
        arg: *mut (),
    ) -> Self {
        let sp = (stack_top.as_ptr() as usize & !0xF) as u64;
        let mut context = Self {
            x19_x28: [0; 10],
            fp: 0,
            lr: thread_entry_shim as *const () as usize as u64,
            sp,
            pstate: 0,
            d8_d15: [0; 8],
        };
        context.x19_x28[0] = arg as usize as u64;
        context.x19_x28[1] = entry as usize as u64;
        context
    }

    unsafe fn start(next: &Self) -> ! {
        // SAFETY: используется только для первого входа в поток из boot context.
        unsafe { context_start(next as *const Self) }
    }

    unsafe fn switch(prev: &mut Self, next: &Self) {
        // SAFETY: вызывается scheduler при эксклюзивном владении обоими контекстами.
        unsafe { context_switch(prev as *mut Self, next as *const Self) };
    }
}

#[unsafe(naked)]
unsafe extern "C" fn thread_entry_shim() -> ! {
    core::arch::naked_asm!("mov x0, x19", "br x20",)
}
