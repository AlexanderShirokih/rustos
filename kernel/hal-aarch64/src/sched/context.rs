use core::{mem::offset_of, ptr::NonNull};

use kbase::{CriticalSection as _, DaifCriticalSection};
use memory::{
    memory_mapper::AddressSpaceHandle, physical_address::PhysicalAddress,
    virtual_address::VirtualAddress,
};
use scheduler::{ArchContext, UserEntry};

use super::{
    cpu_local::Aarch64Cpu,
    stack::Aarch64Stack,
    switch::{context_start, context_switch},
};
use crate::{
    exception::{ExceptionFrame, exception_return, gpreg::GpReg},
    memory::{
        asid::unpack_asid,
        regs::{
            common::EL1,
            tcr::TranslationControlRegister,
            ttbr::{LowerHalf, TranslationTableBaseRegister},
        },
    },
};

type Tcr = TranslationControlRegister<EL1>;
type Ttbr0 = TranslationTableBaseRegister<EL1, LowerHalf>;

#[repr(C)]
pub(crate) struct Aarch64Context {
    pub x19_x28: [GpReg; 10],
    pub fp: GpReg,
    pub lr: GpReg,
    pub sp: u64,
    pub nzcv: u64,
    pub daif: u64,
    pub d8_d15: [u64; 8],
    pub ipc_buffer_va: u64,
}

impl ArchContext for Aarch64Context {
    type Cpu = Aarch64Cpu;
    type Stack = Aarch64Stack;

    const USER_VA_END: usize = syscall::USER_VA_END as usize;

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
            ipc_buffer_va: 0,
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
        let _irq_mask = DaifCriticalSection::enter();

        match next {
            None => {
                // SAFETY: IRQ замаскированы до конца функции. `disable_walks` запрещает обход по
                // TTBR0 до записи нулевого корня. Нулевой TTBR0 содержит зарезервированный ASID 0,
                // и TLB-записи user-AS, все non-global, при нём не используются.
                unsafe {
                    Tcr::disable_walks::<LowerHalf>();
                    Ttbr0::set_with_asid(PhysicalAddress::new(0), 0);
                }
            }
            Some(handle) => {
                let asid = unpack_asid(handle.tag.raw());
                // SAFETY: `handle` - корень живого AS с его ASID, и IRQ замаскированы до конца
                // функции. `set_with_asid` применяет корень до разрешения обхода, так что обход
                // по прежнему корню не начинается. ASID-теги изолируют записи прошлого AS,
                // full-flush не требуется.
                unsafe {
                    Ttbr0::set_with_asid(handle.root, asid);
                    Tcr::enable_walks::<LowerHalf>();
                }
            }
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
            ipc_buffer_va: 0,
        };
        ctx.x19_x28[0] = GpReg::from_u64(entry.arg.0);
        ctx.x19_x28[1] = GpReg::from_u64(entry.user_pc.as_usize() as u64);
        ctx.x19_x28[2] = GpReg::from_u64(entry.user_sp.as_usize() as u64);
        ctx
    }

    fn set_ipc_buffer(&mut self, va: Option<VirtualAddress>) {
        self.ipc_buffer_va = va.map_or(0, |va| va.as_usize() as u64);
    }
}

#[unsafe(naked)]
unsafe extern "C" fn thread_entry_shim() -> ! {
    core::arch::naked_asm!("mov x0, x19", "br x20",)
}

// SAFETY: вход только из `context_start`, который восстановил callee-saved
// регистры и сделал `ret` сюда: `x19` = `bootstrap_x0` -> user `x0`, `x20` =
// `user_pc` -> `ELR_EL1`, `x21` = `user_sp` -> `SP_EL0`, а текущий `sp` -
// вершина kernel-стека, выровненная на 16. Кадр исключения ложится под неё, и
// `exception_return` восстанавливает из него регистры EL0 так же, как после исключения.
#[unsafe(naked)]
unsafe extern "C" fn el0_entry_shim() -> ! {
    core::arch::naked_asm!(
        // `exception_return` пишет `ELR_EL1/SPSR_EL1` при замаскированных IRQ.
        "msr daifset, #0b0010",
        "sub sp, sp, #{frame_size}",
        // Нулевой кадр задаёт `SPSR_EL1 = 0` (EL0t, AArch64, `DAIF=0`) и обнуляет GPR,
        // SIMD/FP и `tpidr_el0`: kernel-значения и регистры прошлого владельца CPU в EL0 не
        // попадают.
        "mov x9, sp",
        "add x10, sp, #{frame_size}",
        "1:",
        "stp xzr, xzr, [x9], #16",
        "cmp x9, x10",
        "b.ne 1b",
        "str x19, [sp, #{x0}]",
        "str x20, [sp, #{elr}]",
        "str x21, [sp, #{sp_el0}]",
        "b {exception_return}",
        frame_size = const size_of::<ExceptionFrame>(),
        x0 = const offset_of!(ExceptionFrame, regs),
        elr = const offset_of!(ExceptionFrame, elr),
        sp_el0 = const offset_of!(ExceptionFrame, sp_el0),
        exception_return = sym exception_return,
    )
}
