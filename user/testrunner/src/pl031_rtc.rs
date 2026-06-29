//! E2E userspace-драйвера PL031 RTC: минт IRQ-линии и device-MMIO окна по
//! bootstrap-контракту, ожидание реального прерывания и подтверждение.
//! Сквозной критпуть провижнинга устройства из userland.

use bootstrap::BootstrapClient;
use io::mmio::{Mmio, Reg};
use kernel_tests::kernel_test;
use runtime::{IrqControl, MemoryRegion, OwnedHandle, PortTransport, Timeout, UserMemFlags};
use syscall::SIGNALED;

/// PL031 RTC на qemu virt: физбаза MMIO и линия прерывания (GIC_SPI 2 -> INTID 34).
const PL031_BASE: u64 = 0x0901_0000;
const PL031_IRQ: u16 = 34;

/// Текущее значение (секунды), RO.
const RTC_DR: Reg<u32> = Reg::new(0x00);
/// Match-регистр.
const RTC_MR: Reg<u32> = Reg::new(0x04);
/// Маска прерывания: 1 = разрешено.
const RTC_IMSC: Reg<u32> = Reg::new(0x10);
/// Очистка прерывания: запись 1.
const RTC_ICR: Reg<u32> = Reg::new(0x1C);

/// Бюджет ожидания.
const WAIT_BUDGET: Timeout = Timeout::from_ns(10_000_000_000);

#[kernel_test]
fn pl031_rtc_interrupt() {
    let client = BootstrapClient::new(PortTransport::client(crate::bootstrap_handle()));

    // Полномочие на линии + минт линии PL031.
    let control = IrqControl::from_handle(OwnedHandle::adopt(
        client.acquire_irq_control().expect("call").expect("vend"),
    ));
    let line = control.mint(PL031_IRQ).expect("mint PL031 line");

    // Device-окно PL031 по базовому PA + отображение. Device-атрибуты ставит
    // ядро из типа региона; userspace выбирает только доступ.
    let region = acquire_window_by_base(&client, PL031_BASE).expect("PL031 device window");
    let info = region.inspect().expect("inspect");
    let mapping = region
        .map(info.size_bytes, UserMemFlags::ReadWrite)
        .expect("map device window");

    // SAFETY: device-память строго volatile (обеспечивает Mmio).
    let mmio = Mmio::new(mapping.va() as usize);

    // Вооружить RTC: match через 2 тика (запас на установку), разрешить прерывание.
    let now = mmio.read_reg(RTC_DR);
    mmio.write_reg(RTC_MR, now.wrapping_add(2));
    mmio.write_reg(RTC_IMSC, 1);

    // Ждать реальное прерывание линии: Ok = маска срабатывания, Err = тайм-аут.
    let observed = line.wait(WAIT_BUDGET).expect("interrupt fired");
    kernel_tests::kassert!(observed & SIGNALED != 0);

    mmio.write_reg(RTC_ICR, 1);
    line.ack().expect("ack");
}

fn acquire_window_by_base(
    client: &BootstrapClient<PortTransport>,
    base: u64,
) -> Option<MemoryRegion> {
    let mut index = 0;
    loop {
        let region = match client.acquire_device_memory(index) {
            Ok(Ok(cap)) => MemoryRegion::from_handle(OwnedHandle::adopt(cap)),
            // Транспортная ошибка либо индекс вне набора - окна кончились.
            _ => return None,
        };

        if region.inspect().is_ok_and(|info| info.base == base) {
            return Some(region);
        }
        index += 1;
    }
}
