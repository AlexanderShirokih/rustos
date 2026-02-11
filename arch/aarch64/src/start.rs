#![no_std]
#![no_main]
extern crate alloc;

#[cfg(test)]
extern crate std;

mod boot_header;
mod exception;
mod memory;
mod system;

extern crate drivers_common_aarch64;

use crate::memory::layout::MemoryRegion;
use crate::memory::memory_setup::{Early, Installed, MemoryManagerResult, MemorySetup};
use crate::memory::setup::build_memory_layout;
use ::memory::physical_address::PageAlignedAddress;
use ::memory::virtual_address::{PageAlignedVirtualAddress, VirtualAddress};
use alloc::boxed::Box;
use alloc::vec::Vec;
use arch_common::scanner;
use core::arch::naked_asm;
use core::hint::spin_loop;
use drivers_common::early::EarlyDriverRegistry;
use drivers_common::scanner::DriverScanner;
use drivers_common_aarch64::adapt_tree;
use fdt::devicetree::{DeviceTree, NodeKey};
use kernel::kernel_context::KernelContext;
use kernel::kmain::kmain;
use klog::{debug, fatal, set_early_stdout};

/// База higher half (верхней половины адресного пространства).
pub const HIGHER_HALF_BASE: usize = 0xFFFF_FF80_0000_0000;

unsafe extern "C" {
    static _stack_top: u8;
    static _kernel_start: u8;
    static _bss_start: u8;
    static _bss_end: u8;
}

#[unsafe(no_mangle)]
#[unsafe(naked)]
pub extern "C" fn _start() -> () {
    naked_asm!(
    // Сохранение DTB (x0) в callee-saved регистре
    "mov    x19, x0",

    // Проверяем EL
    "mrs    x0, CurrentEL",
    "cmp    x0, #0x8",          // EL2?
    "b.ne   1f",                // если не EL2, то идем сразу в EL1

    // Обработчик EL2

    // HCR_EL2: EL1 в AArch64
    "mov    x0, #(1 << 31)",
    "msr    hcr_el2, x0",

    // Отключение ловушек SIMD/FP
    "mov    x0, #0x33ff",
    "msr    cptr_el2, x0",

    // SPSR_EL2: возврат в EL1h, DAIF masked
    "mov    x0, #0x3c5",
    "msr    spsr_el2, x0",
    "adr    x0, 1f",
    "msr    elr_el2, x0",
    "eret",

    // Обработчик EL1
    "1:",

    // Инициализация SP_EL1
    "msr    spsel, #1",
    "adrp   x1, {stack_top}",
    "add    x1, x1, #:lo12:{stack_top}",
    "mov    sp, x1",

    // Включение FP/SIMD
    "mrs    x0, cpacr_el1",
    "orr    x0, x0, #(0x3 << 20)",
    "msr    cpacr_el1, x0",
    "isb",

    // Очистка BSS
    "adrp   x0, {bss_start}",
    "add    x0, x0, #:lo12:{bss_start}",
    "adrp   x1, {bss_end}",
    "add    x1, x1, #:lo12:{bss_end}",
    "cmp    x0, x1",
    "b.ge   11f",

    "10:",
    "str    xzr, [x0], #8",
    "cmp    x0, x1",
    "b.lt   10b",
    "11:",

    // Восстанавливаем DTB
    "mov    x0, x19",

    // Переход в Rust
    "b      {early_main}",

    // Если вернулись — уходим в WaitForEvent
    "msr    daifset, #0b0010",
    "20:",
    "wfe",
    "b      20b",

    bss_start = sym _bss_start,
    bss_end   = sym _bss_end,
    stack_top = sym _stack_top,
    early_main = sym early_main0,
    )
}

fn early_main0(dtb: usize) {
    let _ = early_main(dtb);
}

/// Ранняя инициализация ядра
fn early_main(dtb: usize) -> Result<(), ()> {
    // Установка векторов исключений
    let vectors = exception::ExceptionVectors::instance();
    vectors.install();

    // Парсим DTB
    let device_tree = DeviceTree::from_ptr(dtb).map_err(|_| {})?;

    // Извлекаем из DTB информацию о регионах памяти.
    let memory_layout = build_memory_layout(&device_tree).map_err(|_| {})?;

    // Установка раннего bump аллокатора
    let early_setup = MemorySetup::<Early>::create(memory_layout).map_err(|_| {})?;
    let early_setup = early_setup.install();

    // Инициализация ранних драйверов
    let root = adapt_tree(&device_tree).root().ok_or(())?;
    let early_drivers = drivers_aarch64::early_drivers();
    let mut early_registry = EarlyDriverRegistry::<NodeKey>::new();
    early_registry.scan_and_probe(root, early_drivers);

    // Сохраняем реестр в статическую память
    let early_registry: &'static EarlyDriverRegistry<NodeKey> = Box::leak(Box::new(early_registry));
    bind_early_stdout(&device_tree, early_registry);

    let mmio_requests: Vec<_> = early_registry
        .mmio_region_requests()
        .iter()
        .map(|mmio_request| {
            MemoryRegion::mmio(mmio_request.address.base(), mmio_request.address.size())
        })
        .collect();

    let result = setup_memory(mmio_requests, early_setup)?;

    // Релокация векторов исключений в higher half
    unsafe { vectors.relocated(VirtualAddress::new(HIGHER_HALF_BASE)) }.install();

    let mut driver_scanner = DriverScanner::new();

    let root_node = adapt_tree(&device_tree)
        .root()
        .expect("Device Tree root node is missing!");

    // Инициализация драйверов
    let drivers = drivers_aarch64::runtime_drivers();
    driver_scanner.scan_and_probe(root_node, drivers);

    // Основная платформозависимая настройка завершена. Переходим к общей точке входа
    let kernel = KernelContext::new(result.memory_mapper);
    let kernel = Box::leak(Box::new(kernel));

    kmain(driver_scanner, kernel);

    loop {
        spin_loop();
    }
}

/// Настраивает MMU и переключает ядро в higher half.
fn setup_memory(
    mmio: Vec<MemoryRegion<PageAlignedAddress>>,
    installed: MemorySetup<Installed>,
) -> Result<MemoryManagerResult, ()> {
    // Переход из Installed в Prepared фазу с резервированием bump region
    let memory_setup = installed
        .prepare(mmio)
        .inspect_err(|err| fatal!("Memory setup failed: {:?}", err))
        .map_err(|_| ())?;

    // Маппинг higher half и включение MMU
    let higher_half_base =
        PageAlignedVirtualAddress::new_unchecked(VirtualAddress::new(HIGHER_HALF_BASE));
    let memory_setup = memory_setup
        .enable(higher_half_base)
        .inspect_err(|err| fatal!("Unable to enable MMU: {:?}", err))
        .map_err(|_| ())?;

    debug!("MMU enabled!");

    // Прыжок в higher half — после этого PC указывает на HIGHER_HALF_BASE + PA
    unsafe { jump_to_higher_half() };

    debug!("Running in higher half");

    let result = memory_setup
        .install()
        .inspect(|_| debug!("Global allocator switched to heap phase"))
        .inspect_err(|_| fatal!("Unable to set heap allocator"))?;

    Ok(result)
}

/// Прыжок с identity адреса на higher half адрес.
/// После этого PC указывает на HIGHER_HALF_BASE + текущий PA.
#[unsafe(naked)]
unsafe extern "C" fn jump_to_higher_half() {
    // Разбиваем HIGHER_HALF_BASE на 16-битные части для movz/movk
    const HALF_47_32: u64 = ((HIGHER_HALF_BASE as u64) >> 32) & 0xFFFF;
    const HALF_63_48: u64 = ((HIGHER_HALF_BASE as u64) >> 48) & 0xFFFF;

    naked_asm!(
        "adr x0, 1f",                      // x0 = адрес метки 1 (identity)
        "movz x1, #{half_32}, lsl #32",    // x1[47:32]
        "movk x1, #{half_48}, lsl #48",    // x1[63:48]
        "add x0, x0, x1",                  // x0 = identity + HIGHER_HALF_BASE
        "br x0",                           // Прыжок на higher half адрес
        "1:",                              // Метка — сюда придём уже по higher half адресу
        "ret",
        half_32 = const HALF_47_32,
        half_48 = const HALF_63_48,
    )
}

/// Привязывает stdout к консольному драйверу из DeviceTree.
fn bind_early_stdout(device_tree: &DeviceTree, registry: &'static EarlyDriverRegistry<NodeKey>) {
    let early_console_node = scanner::find_console(device_tree);

    let writer = early_console_node
        .map(|node| node.key())
        .and_then(|key| registry.get(&key))
        .and_then(|driver| driver.output());

    if let Some(writer) = writer {
        set_early_stdout(writer);
    }
}

// В no_std окружении требуется свой panic handler
#[cfg(not(test))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    unsafe {
        fatal!("Kernel panic: {}", info);

        loop {
            core::arch::asm!("wfi", options(nomem, nostack));
        }
    }
}
