//! E2E userspace-драйвера PL031 RTC: минт IRQ-линии и device-MMIO окна по
//! bootstrap-контракту, ожидание реального прерывания и подтверждение.
//! Сквозной критпуть провижнинга устройства из userspace.

use bootstrap::BootstrapClient;
use io::mmio::{Mmio, Reg};
use kernel_tests::kernel_test;
use runtime::{
    MemoryRegion, OwnedHandle, PortTransport, UserMemFlags, handle_close, irq_ack, irq_mint,
    signal_wait_one,
};
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
const WAIT_BUDGET_NS: u64 = 10_000_000_000;

#[kernel_test]
fn pl031_rtc_interrupt() {
    let client = BootstrapClient::new(PortTransport::client(crate::bootstrap_handle()));

    // Полномочие на линии + минт линии PL031.
    let control = OwnedHandle::adopt(client.acquire_irq_control().expect("call").expect("vend"));
    let line = irq_mint(control.as_raw(), PL031_IRQ).expect("mint PL031 line");

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

    // Ждать реальное прерывание линии. Отрицательный возврат -> 0 -> провал проверки.
    let observed = u32::try_from(signal_wait_one(line, SIGNALED, WAIT_BUDGET_NS)).unwrap_or(0);
    kernel_tests::kassert!(observed & SIGNALED != 0);

    mmio.write_reg(RTC_ICR, 1);
    kernel_tests::kassert!(irq_ack(line) == 0);

    // Освободить линию.
    let _ = handle_close(line);
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
