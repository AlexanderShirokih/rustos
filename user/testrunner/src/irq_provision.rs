//! E2E выдачи IRQ-полномочия: `acquire_irq_control` по bootstrap-контракту, усыновление
//! доставленного capability и минт `IrqLine`.
//! Сквозной прогон пути выдачи vend -> transfer -> install -> use из userspace.

use bootstrap::BootstrapClient;
use kernel_tests::kernel_test;
use runtime::{IrqControl, OwnedHandle, PortTransport};

/// PL031 RTC на qemu virt: `GIC_SPI 2` -> INTID 34.
const PL031_IRQ: u16 = 34;

#[kernel_test]
fn acquire_irq_control_and_mint_line() {
    let client = BootstrapClient::new(PortTransport::client(crate::bootstrap_handle()));

    let cap = client
        .acquire_irq_control()
        .expect("acquire_irq_control call")
        .expect("vend ok");
    let control = IrqControl::from_handle(OwnedHandle::adopt(cap));

    // Успешный минт доказывает, что доставленный capability установлен ядром в таблицу процесса.
    let line = control.mint(PL031_IRQ).expect("mint IrqLine");

    // Свежая линия: подтверждать нечего, ack -> ошибка. Заодно подтверждает,
    // что минт вернул живой `IrqLine`. Линия освобождается дропом `line`.
    kernel_tests::kassert!(line.ack().is_err());
}
