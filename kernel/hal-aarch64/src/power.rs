//! Выключение машины.
//!
//! Ядро, собранное с фичей `power-semihosting`, выключает машину semihosting-вызовом `SYS_EXIT`,
//! который передаёт код завершения хосту QEMU. Без этой фичи ядро при любом коде завершения
//! выключает машину вызовом PSCI `SYSTEM_OFF` по соглашению из узла `/psci`.

use core::{
    arch::asm,
    sync::atomic::{AtomicBool, Ordering},
};

use fdt::devicetree::DeviceTree;
use hal_common::smp::PsciConduit;
use klog::warn;
use spin::Once;

use crate::psci::{PsciCall, psci_call};

const ADP_STOPPED_APPLICATION_EXIT: u64 = 0x2_0026;
const SYS_EXIT: u32 = 0x18;

static PSCI_CONDUIT: Once<PsciConduit> = Once::new();
static PSCI_SYSTEM_OFF_ISSUED: AtomicBool = AtomicBool::new(false);

/// Регистрирует выключение машины в `kernelspace::power`.
///
/// Без фичи `power-semihosting` выключение требует узла `/psci` с известным `method`. Без такого
/// узла регистрация не выполняется, и `system_off` остаётся в цикле ожидания.
pub fn install(device_tree: &DeviceTree<'_>) {
    if cfg!(feature = "power-semihosting") {
        kernelspace::power::install(semihosting_exit);
        return;
    }

    match hal_common::smp::psci_conduit(device_tree) {
        Some(conduit) => {
            PSCI_CONDUIT.call_once(|| conduit);
            kernelspace::power::install(psci_system_off);
        }
        None => warn!("power: device tree declares no PSCI conduit, shutdown will not power off"),
    }
}

fn semihosting_exit(code: i32) -> ! {
    let status = if code == 0 {
        0
    } else {
        u32::try_from(code).map_or(1, |c| c.clamp(1, 255))
    };

    // На AArch64 SYS_EXIT принимает указатель на блок { reason, status }.
    let params: [u64; 2] = [ADP_STOPPED_APPLICATION_EXIT, u64::from(status)];
    let params_ptr = core::ptr::addr_of!(params).cast::<u64>();

    // SAFETY: `params` остаётся на стеке и в области видимости до конца вызова; w0/x1 задают
    // ABI semihosting SYS_EXIT - номер операции и адрес блока `{reason, status}`. `hlt #0xf000`
    // не имеет иных эффектов на регистры или память, кроме описанных вызовом
    // (nostack, preserves_flags). Фичу `power-semihosting` объявляют только спецификации, которые
    // запускают QEMU с `-semihosting`, и QEMU обрабатывает `hlt #0xf000` как вызов.
    unsafe {
        asm!(
            "hlt #0xf000",
            in("w0") SYS_EXIT,
            in("x1") params_ptr,
            options(nostack, preserves_flags),
        );
    }

    wait_forever()
}

fn psci_system_off(_code: i32) -> ! {
    // Вызов выполняется один раз. Повторный вход возникает на другом CPU либо из паники по
    // исключению первого вызова, и второй вызов в последнем случае повторил бы то же исключение.
    if !PSCI_SYSTEM_OFF_ISSUED.swap(true, Ordering::Relaxed)
        && let Some(&conduit) = PSCI_CONDUIT.get()
    {
        // Ненулевой код завершения тоже ведёт к `SYSTEM_OFF`. PSCI не передаёт код прошивке, так
        // что `SYSTEM_RESET` не сообщил бы причину отказа. Сброс заменил бы последний вывод UART
        // выводом новой загрузки, а образ, загруженный `fastboot boot`, - установленной системой.
        // `SYSTEM_OFF` определён с PSCI 0.2. Прошивка PSCI 0.1 возвращает управление, и CPU
        // остаётся в цикле `wfi`.
        //
        // SAFETY: `SYSTEM_OFF` не принимает аргументов, а соглашение о вызове объявлено узлом
        // `/psci` device tree.
        unsafe { psci_call(conduit, PsciCall::SystemOff) };
    }

    wait_forever()
}

fn wait_forever() -> ! {
    loop {
        // SAFETY: wfi - стандартная инструкция ожидания, без побочных эффектов.
        unsafe {
            asm!("wfi", options(nomem, nostack, preserves_flags));
        }
    }
}
