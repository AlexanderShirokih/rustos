use super::context::Aarch64Context;

/// Сохраняет полное callee-saved состояние `prev` и восстанавливает `next`.
///
/// Layout структуры зафиксирован в [`Aarch64Context`]: GPR x19..x28, fp, lr,
/// sp, pstate (NZCV), d8..d15. Любые изменения должны синхронизироваться с
/// константами `CTX_OFFSET_*`.
#[unsafe(naked)]
pub unsafe extern "C" fn context_switch(_prev: *mut Aarch64Context, _next: *const Aarch64Context) {
    core::arch::naked_asm!(
        // Save GPR callee-saved
        "stp x19, x20, [x0, #0]",
        "stp x21, x22, [x0, #16]",
        "stp x23, x24, [x0, #32]",
        "stp x25, x26, [x0, #48]",
        "stp x27, x28, [x0, #64]",
        "stp x29, x30, [x0, #80]",
        "mov x9, sp",
        "str x9, [x0, #96]",
        "mrs x9, nzcv",
        "str x9, [x0, #104]",
        // Save FP/SIMD callee-saved (low 64 bits only per AAPCS64)
        "stp d8,  d9,  [x0, #112]",
        "stp d10, d11, [x0, #128]",
        "stp d12, d13, [x0, #144]",
        "stp d14, d15, [x0, #160]",
        // Restore next
        "ldp x19, x20, [x1, #0]",
        "ldp x21, x22, [x1, #16]",
        "ldp x23, x24, [x1, #32]",
        "ldp x25, x26, [x1, #48]",
        "ldp x27, x28, [x1, #64]",
        "ldp x29, x30, [x1, #80]",
        "ldr x9, [x1, #96]",
        "mov sp, x9",
        "ldr x9, [x1, #104]",
        "msr nzcv, x9",
        "ldp d8,  d9,  [x1, #112]",
        "ldp d10, d11, [x1, #128]",
        "ldp d12, d13, [x1, #144]",
        "ldp d14, d15, [x1, #160]",
        "ret",
    )
}

/// Безусловный прыжок в `next` без сохранения текущего контекста.
/// Используется только для запуска первого потока scheduler-а.
#[unsafe(naked)]
pub unsafe extern "C" fn context_start(_next: *const Aarch64Context) -> ! {
    core::arch::naked_asm!(
        "ldp x19, x20, [x0, #0]",
        "ldp x21, x22, [x0, #16]",
        "ldp x23, x24, [x0, #32]",
        "ldp x25, x26, [x0, #48]",
        "ldp x27, x28, [x0, #64]",
        "ldp x29, x30, [x0, #80]",
        "ldr x9, [x0, #96]",
        "mov sp, x9",
        "ldr x9, [x0, #104]",
        "msr nzcv, x9",
        "ldp d8,  d9,  [x0, #112]",
        "ldp d10, d11, [x0, #128]",
        "ldp d12, d13, [x0, #144]",
        "ldp d14, d15, [x0, #160]",
        "ret",
    )
}
