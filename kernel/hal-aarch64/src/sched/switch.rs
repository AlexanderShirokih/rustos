use core::mem::offset_of;

use super::context::Aarch64Context;

const _: () = assert!(offset_of!(Aarch64Context, lr) == offset_of!(Aarch64Context, fp) + 8);

#[unsafe(naked)]
pub(crate) unsafe extern "C" fn context_switch(
    _prev: *mut Aarch64Context,
    _next: *const Aarch64Context,
) {
    core::arch::naked_asm!(
        // Save GPR callee-saved
        "stp x19, x20, [x0, #{regs}]",
        "stp x21, x22, [x0, #({regs} + 16)]",
        "stp x23, x24, [x0, #({regs} + 32)]",
        "stp x25, x26, [x0, #({regs} + 48)]",
        "stp x27, x28, [x0, #({regs} + 64)]",
        "stp x29, x30, [x0, #{fp}]",
        "mov x9, sp",
        "str x9, [x0, #{sp}]",
        "mrs x9, nzcv",
        "str x9, [x0, #{nzcv}]",
        "mrs x9, daif",
        "str x9, [x0, #{daif}]",
        // Save FP/SIMD callee-saved (low 64 bits only per AAPCS64)
        "stp d8,  d9,  [x0, #{d8}]",
        "stp d10, d11, [x0, #({d8} + 16)]",
        "stp d12, d13, [x0, #({d8} + 32)]",
        "stp d14, d15, [x0, #({d8} + 48)]",
        // Restore next
        "ldp x19, x20, [x1, #{regs}]",
        "ldp x21, x22, [x1, #({regs} + 16)]",
        "ldp x23, x24, [x1, #({regs} + 32)]",
        "ldp x25, x26, [x1, #({regs} + 48)]",
        "ldp x27, x28, [x1, #({regs} + 64)]",
        "ldp x29, x30, [x1, #{fp}]",
        "ldr x9, [x1, #{sp}]",
        "mov sp, x9",
        "ldr x9, [x1, #{nzcv}]",
        "msr nzcv, x9",
        "ldr x9, [x1, #{daif}]",
        "msr daif, x9",
        "ldp d8,  d9,  [x1, #{d8}]",
        "ldp d10, d11, [x1, #({d8} + 16)]",
        "ldp d12, d13, [x1, #({d8} + 32)]",
        "ldp d14, d15, [x1, #({d8} + 48)]",
        "ret",
        regs = const offset_of!(Aarch64Context, x19_x28),
        fp = const offset_of!(Aarch64Context, fp),
        sp = const offset_of!(Aarch64Context, sp),
        nzcv = const offset_of!(Aarch64Context, nzcv),
        daif = const offset_of!(Aarch64Context, daif),
        d8 = const offset_of!(Aarch64Context, d8_d15),
    )
}

#[unsafe(naked)]
pub(crate) unsafe extern "C" fn context_start(_next: *const Aarch64Context) -> ! {
    core::arch::naked_asm!(
        "ldp x19, x20, [x0, #{regs}]",
        "ldp x21, x22, [x0, #({regs} + 16)]",
        "ldp x23, x24, [x0, #({regs} + 32)]",
        "ldp x25, x26, [x0, #({regs} + 48)]",
        "ldp x27, x28, [x0, #({regs} + 64)]",
        "ldp x29, x30, [x0, #{fp}]",
        "ldr x9, [x0, #{sp}]",
        "mov sp, x9",
        "ldr x9, [x0, #{nzcv}]",
        "msr nzcv, x9",
        "ldr x9, [x0, #{daif}]",
        "msr daif, x9",
        "ldp d8,  d9,  [x0, #{d8}]",
        "ldp d10, d11, [x0, #({d8} + 16)]",
        "ldp d12, d13, [x0, #({d8} + 32)]",
        "ldp d14, d15, [x0, #({d8} + 48)]",
        "ret",
        regs = const offset_of!(Aarch64Context, x19_x28),
        fp = const offset_of!(Aarch64Context, fp),
        sp = const offset_of!(Aarch64Context, sp),
        nzcv = const offset_of!(Aarch64Context, nzcv),
        daif = const offset_of!(Aarch64Context, daif),
        d8 = const offset_of!(Aarch64Context, d8_d15),
    )
}
