//! E2E userspace-драйвера PL031 RTC: параметры устройства из boot FDT, минт
//! IRQ-линии и device-MMIO окна по bootstrap-контракту, ожидание реального
//! прерывания и подтверждение. Сквозной критпуть провижнинга устройства из
//! userland без захардкоженных адресов платы.

use bootstrap::BootstrapClient;
use fdt::{
    devicetree::{DeviceTree, Node},
    devicetreeext::NodeExt,
};
use io::mmio::Reg;
use kernel_tests::kernel_test;
use runtime::{IrqControl, MemoryRegion, OwnedHandle, PortTransport, Timeout, UserMemFlags};
use syscall::SIGNALED;

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

    // База MMIO и линия прерывания PL031 - из boot FDT.
    let fdt_region = MemoryRegion::adopt(client.acquire_boot_fdt().expect("call").expect("vend"));
    let fdt_mapping = fdt_region
        .map_full(UserMemFlags::ReadOnly)
        .expect("map fdt region");
    let tree = DeviceTree::from_bytes(fdt_mapping.as_bytes()).expect("parse dtb");
    let root = tree.root().expect("dtb root node");
    let pl031 = root
        .children()
        .find(|node| node.is_compatible("arm,pl031"))
        .expect("pl031 node in dtb");
    let base = pl031.first_reg_base(&root).expect("pl031 reg window");
    let intid = gic_spi_intid(&pl031).expect("pl031 SPI line");

    // Полномочие на линии + минт линии PL031.
    let control = IrqControl::from_handle(OwnedHandle::adopt(
        client.acquire_irq_control().expect("call").expect("vend"),
    ));
    let line = control.mint(intid).expect("mint PL031 line");

    // Device-окно PL031 по базовому PA + отображение. Device-атрибуты ставит
    // ядро из типа региона; userspace выбирает только доступ.
    let region: MemoryRegion = client
        .acquire_device_memory_by_base(base)
        .expect("bootstrap call")
        .expect("PL031 device window");
    let mapping = region
        .map_full(UserMemFlags::ReadWrite)
        .expect("map device window");
    let mmio = mapping.mmio();

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

/// INTID из GIC-формата `interrupts` (`<type num flags>`): SPI -> 32 + num.
fn gic_spi_intid(node: &Node<'_>) -> Option<u16> {
    const GIC_SPI: u32 = 0;
    const SPI_INTID_BASE: u32 = 32;
    let interrupts = node.prop("interrupts")?;
    if interrupts.try_as_u32(0)? != GIC_SPI {
        return None;
    }
    u16::try_from(SPI_INTID_BASE + interrupts.try_as_u32(4)?).ok()
}
