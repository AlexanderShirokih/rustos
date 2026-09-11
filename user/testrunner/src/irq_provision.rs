//! E2E выдачи IRQ-полномочия: `acquire_irq_control` по bootstrap-контракту, усыновление
//! доставленного capability и минт `IrqLine`.
//! Сквозной прогон пути выдачи vend -> transfer -> install -> use из userspace.

use bootstrap_abi::bootstrap;
use kernel_objects::{Error, IrqControl};
use kernel_tests::kernel_test;
use runtime::PortTransport;
use syscall::{IrqTrigger, SyscallError};

const SPI_BASE: u16 = 32;

const PL031_SPI: u16 = 2;

const PL031_IRQ: u16 = SPI_BASE + PL031_SPI;

const SPARE_IRQ: u16 = 47;

const SPI_MAX: u16 = 1019;

const PRIVATE_IRQ_MAX: u16 = SPI_BASE - 1;

#[kernel_test]
fn acquire_irq_control_and_mint_line() {
    let client = bootstrap::Client::new(PortTransport::new(crate::bootstrap_handle()));

    let cap = client
        .acquire_irq_control()
        .expect("acquire_irq_control call")
        .expect("vend ok");
    let control = IrqControl::from(cap);

    // Успешный минт доказывает, что доставленный capability установлен ядром в таблицу процесса.
    let line = control
        .mint(PL031_IRQ, IrqTrigger::Level)
        .expect("mint IrqLine");

    // Свежая линия: подтверждать нечего, ack -> ошибка. Заодно подтверждает,
    // что минт вернул живой `IrqLine`. Линия освобождается дропом `line`.
    kernel_tests::kassert!(line.ack().is_err());
}

#[kernel_test]
fn minting_a_line_twice_names_the_taken_line() {
    let client = bootstrap::Client::new(PortTransport::new(crate::bootstrap_handle()));
    let cap = client
        .acquire_irq_control()
        .expect("acquire_irq_control call")
        .expect("vend ok");
    let control = IrqControl::from(cap);
    let line = control
        .mint(SPARE_IRQ, IrqTrigger::Level)
        .expect("mint IrqLine");

    let taken = control.mint(SPARE_IRQ, IrqTrigger::Level);

    // Занятая линия - отдельный отказ. Исчерпание бюджета памяти к ней отношения не имеет.
    kernel_tests::kassert_eq!(
        taken.err(),
        Some(Error::Syscall(SyscallError::AlreadyBound))
    );
    drop(line);
}

fn first_line_outside(control: &IrqControl) -> u16 {
    // Полоса - диапазон, поэтому отказы монотонны и край берётся делением
    // пополам; занятая линия внутри полосы отвечает `AlreadyBound`.
    let mut inside = PL031_IRQ;
    let mut outside = SPI_MAX + 1;
    while outside - inside > 1 {
        let probe = inside + (outside - inside) / 2;
        match control.mint(probe, IrqTrigger::Level) {
            Err(Error::Syscall(SyscallError::AccessDenied)) => outside = probe,
            outcome => {
                drop(outcome);
                inside = probe;
            }
        }
    }
    outside
}

#[kernel_test]
fn irq_band_ends_at_the_platform_line_count() {
    let client = bootstrap::Client::new(PortTransport::new(crate::bootstrap_handle()));
    let cap = client
        .acquire_irq_control()
        .expect("acquire_irq_control call")
        .expect("vend ok");
    let control = IrqControl::from(cap);

    let outside = first_line_outside(&control);

    // Край полосы совпадает с числом линий контроллера, и на qemu virt их меньше
    // архитектурного предела. Край ровно в SPI_MAX + 1 отмечает полосу-литерал.
    kernel_tests::kassert!(outside > PL031_IRQ);
    kernel_tests::kassert!(outside < SPI_MAX + 1);
    kernel_tests::kassert_eq!(
        control.mint(outside, IrqTrigger::Level).err(),
        Some(Error::Syscall(SyscallError::AccessDenied))
    );
}

#[kernel_test]
fn irq_band_starts_above_the_private_lines() {
    let client = bootstrap::Client::new(PortTransport::new(crate::bootstrap_handle()));
    let cap = client
        .acquire_irq_control()
        .expect("acquire_irq_control call")
        .expect("vend ok");
    let control = IrqControl::from(cap);

    // Нижний край полосы архитектурно фиксирован SPI_BASE: `PRIVATE_IRQ_MAX` -
    // последняя приватная линия CPU, вне полосы на любой машине.
    kernel_tests::kassert_eq!(
        control.mint(PRIVATE_IRQ_MAX, IrqTrigger::Level).err(),
        Some(Error::Syscall(SyscallError::AccessDenied))
    );
}
