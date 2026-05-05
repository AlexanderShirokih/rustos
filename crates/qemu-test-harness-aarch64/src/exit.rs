//! ARM semihosting `SYS_EXIT`.

#[allow(dead_code)]
const ADP_STOPPED_APPLICATION_EXIT: u64 = 0x2_0026;
#[allow(dead_code)]
const SYS_EXIT: u32 = 0x18;

#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub fn semihosting_exit(code: u32) -> ! {
    use core::arch::asm;

    // На AArch64 SYS_EXIT принимает указатель на блок { reason, status };
    // в отличие от AArch32, где status не передаётся.
    let params: [u64; 2] = [ADP_STOPPED_APPLICATION_EXIT, u64::from(code)];
    let params_ptr = core::ptr::addr_of!(params).cast::<u64>();

    // SAFETY: hlt #0xf000 - semihosting trap; под `qemu -semihosting`
    // QEMU читает блок параметров и завершает виртуальную машину.
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

#[cfg(not(all(target_arch = "aarch64", target_os = "none")))]
pub fn semihosting_exit(code: u32) -> ! {
    panic!("semihosting_exit({code}) is not supported on host");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_match_arm_semihosting_spec() {
        assert_eq!(SYS_EXIT, 0x18);
        assert_eq!(ADP_STOPPED_APPLICATION_EXIT, 0x20026);
    }
}
