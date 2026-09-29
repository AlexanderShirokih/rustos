//! E2E выдачи IRQ-полномочия: `acquire_irq_control` по сессии `Hardware`, усыновление
//! доставленного capability и минт `IrqLine`.
//! Сквозной прогон пути выдачи vend -> transfer -> install -> use из userspace.

use kernel_objects::{Error, IrqControl};
use kernel_tests::kernel_test;
use syscall::{IrqTrigger, Rights, SyscallError};

const SPI_BASE: u16 = 32;

const PL031_SPI: u16 = 2;

const PL031_IRQ: u16 = SPI_BASE + PL031_SPI;

const SPARE_IRQ: u16 = 47;

const SPI_MAX: u16 = 1019;

const PRIVATE_IRQ_MAX: u16 = SPI_BASE - 1;

#[kernel_test]
fn acquire_irq_control_and_mint_line() {
    let client = crate::hardware_session().client();

    let cap = client
        .acquire_irq_control()
        .expect("acquire_irq_control call")
        .expect("vend ok");
    let control = IrqControl::from(cap);

    let line = control
        .mint(PL031_IRQ, IrqTrigger::Level)
        .expect("mint IrqLine");

    kernel_tests::kassert!(line.ack().is_err());
}

#[kernel_test]
fn minting_a_line_twice_names_the_taken_line() {
    let client = crate::hardware_session().client();
    let cap = client
        .acquire_irq_control()
        .expect("acquire_irq_control call")
        .expect("vend ok");
    let control = IrqControl::from(cap);
    let line = control
        .mint(SPARE_IRQ, IrqTrigger::Level)
        .expect("mint IrqLine");

    let taken = control.mint(SPARE_IRQ, IrqTrigger::Level);

    kernel_tests::kassert_eq!(
        taken.err(),
        Some(Error::Syscall(SyscallError::AlreadyBound))
    );
    drop(line);
}

#[kernel_test]
fn closing_the_minted_line_frees_it_under_a_live_copy() {
    let client = crate::hardware_session().client();
    let cap = client
        .acquire_irq_control()
        .expect("acquire_irq_control call")
        .expect("vend ok");
    let control = IrqControl::from(cap);
    let line = control
        .mint(SPARE_IRQ, IrqTrigger::Level)
        .expect("mint IrqLine");
    let copy = line
        .duplicate(Rights::READ | Rights::WRITE, None)
        .expect("duplicate IrqLine");

    line.close().expect("close minted line");

    let again = control
        .mint(SPARE_IRQ, IrqTrigger::Level)
        .expect("mint freed line");
    kernel_tests::kassert_eq!(
        copy.ack().err(),
        Some(Error::Syscall(SyscallError::Revoked))
    );
    drop(again);
}

#[kernel_test]
fn closing_a_copy_keeps_the_line_bound() {
    let client = crate::hardware_session().client();
    let cap = client
        .acquire_irq_control()
        .expect("acquire_irq_control call")
        .expect("vend ok");
    let control = IrqControl::from(cap);
    let line = control
        .mint(SPARE_IRQ, IrqTrigger::Level)
        .expect("mint IrqLine");
    let copy = line
        .duplicate(Rights::READ | Rights::WRITE, None)
        .expect("duplicate IrqLine");

    copy.close().expect("close copy");

    kernel_tests::kassert_eq!(
        control.mint(SPARE_IRQ, IrqTrigger::Level).err(),
        Some(Error::Syscall(SyscallError::AlreadyBound))
    );
    drop(line);
}

fn first_line_outside(control: &IrqControl) -> u16 {
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
    let client = crate::hardware_session().client();
    let cap = client
        .acquire_irq_control()
        .expect("acquire_irq_control call")
        .expect("vend ok");
    let control = IrqControl::from(cap);

    let outside = first_line_outside(&control);

    kernel_tests::kassert!(outside > PL031_IRQ);
    kernel_tests::kassert!(outside < SPI_MAX + 1);
    kernel_tests::kassert_eq!(
        control.mint(outside, IrqTrigger::Level).err(),
        Some(Error::Syscall(SyscallError::AccessDenied))
    );
}

#[kernel_test]
fn irq_band_starts_above_the_private_lines() {
    let client = crate::hardware_session().client();
    let cap = client
        .acquire_irq_control()
        .expect("acquire_irq_control call")
        .expect("vend ok");
    let control = IrqControl::from(cap);

    kernel_tests::kassert_eq!(
        control.mint(PRIVATE_IRQ_MAX, IrqTrigger::Level).err(),
        Some(Error::Syscall(SyscallError::AccessDenied))
    );
}
