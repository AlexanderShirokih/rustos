//! Останов машины через ARM semihosting.

const ADP_STOPPED_APPLICATION_EXIT: u64 = 0x2_0026;
const SYS_EXIT: u32 = 0x18;

/// Останавливает машину через semihosting `SYS_EXIT` с кодом `code`.
pub fn machine_off(code: u32) -> ! {
    use core::arch::asm;

    // На AArch64 SYS_EXIT принимает указатель на блок { reason, status }.
    let params: [u64; 2] = [ADP_STOPPED_APPLICATION_EXIT, u64::from(code)];
    let params_ptr = core::ptr::addr_of!(params).cast::<u64>();

    // SAFETY: `params` остаётся на стеке и в области видимости до конца вызова; w0/x1 задают
    // ABI semihosting SYS_EXIT - номер операции и адрес блока `{reason, status}`. `hlt #0xf000`
    // не имеет иных эффектов на регистры или память, кроме описанных вызовом
    // (nostack, preserves_flags). Без host-отладчика, перехватывающего semihosting,
    // управление возвращается на следующую инструкцию, и код ниже уходит в `wfi`.
    unsafe {
        asm!(
            "hlt #0xf000",
            in("w0") SYS_EXIT,
            in("x1") params_ptr,
            options(nostack, preserves_flags),
        );
    }

    loop {
        // SAFETY: wfi - стандартная инструкция ожидания, без побочных эффектов.
        unsafe {
            asm!("wfi", options(nomem, nostack, preserves_flags));
        }
    }
}

/// Адаптер `fn(i32) -> !` для `kernelspace::power`: 0 остаётся 0,
/// прочие коды зажимаются в 1..=255.
pub fn machine_off_exit_code(code: i32) -> ! {
    let machine_code = if code == 0 {
        0
    } else {
        u32::try_from(code).map_or(1, |c| c.clamp(1, 255))
    };
    machine_off(machine_code)
}
