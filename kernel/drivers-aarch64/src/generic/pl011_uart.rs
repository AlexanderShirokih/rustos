//! `ByteSink`/`Writer` поверх регистров UART PL011 (ARM PrimeCell) и boot-`Driver`,
//! который мапит их и публикует получившийся writer консолью ядра.

use alloc::{boxed::Box, sync::Arc};
use core::hint::spin_loop;

use drivers_common::{
    BootServices, Driver, DriverFactory, DriverRunError, Owners, UncachedPermission,
    probe::{ProbeError, ProbeResult},
    services::mmio::{MmioAddress, MmioBound},
};
use drivers_common_aarch64::{FdtProbeContext, ProbeContextExt};
use io::{
    byte_sink::{ByteSink, Pending},
    crlf::Crlf,
    mmio::Reg,
    writer::Writer,
};

use crate::register_driver;

// Регистры PL011

const DR: Reg<u32> = Reg::new(0x00);
const FR: Reg<u32> = Reg::new(0x18);
const CR: Reg<u32> = Reg::new(0x30);

// Биты FR

const FR_TXFF: u32 = 1 << 5;
const FR_BUSY: u32 = 1 << 3;

// Биты CR

const CR_UARTEN: u32 = 1 << 0;
const CR_TXE: u32 = 1 << 8;
const CR_RXE: u32 = 1 << 9;

const REG_UART_INDEX: usize = 0;

/// Пишет байты в MMIO-регистры PL011, блокируясь на переполненном TX FIFO и
/// заменяя `'\n'` на `"\r\n"` через [`Crlf`].
pub struct UartPl011 {
    mmio: MmioBound,
}

impl UartPl011 {
    pub fn new(mmio: MmioBound) -> Self {
        Self { mmio }
    }
}

impl ByteSink for UartPl011 {
    fn try_write(&self, b: u8) -> Result<(), Pending> {
        if (self.mmio.read_reg(FR) & FR_TXFF) != 0 {
            return Err(Pending);
        }

        self.mmio.write_reg(DR, u32::from(b));
        Ok(())
    }

    fn try_write_slice(&self, buf: &[u8]) -> Result<usize, Pending> {
        if buf.is_empty() {
            return Ok(0);
        }

        let mut crlf = Crlf::new(buf);
        let mut consumed = 0usize;

        while let Some(byte) = crlf.next() {
            if (self.mmio.read_reg(FR) & FR_TXFF) != 0 {
                if consumed == 0 {
                    return Err(Pending);
                }
                while (self.mmio.read_reg(FR) & FR_TXFF) != 0 {
                    spin_loop();
                }
            }

            self.mmio.write_reg(DR, u32::from(byte));
            consumed = crlf.consumed();
        }

        Ok(consumed)
    }

    fn flush(&self) {
        while (self.mmio.read_reg(FR) & FR_BUSY) != 0 {
            spin_loop();
        }
    }
}

impl Writer for UartPl011 {
    fn write_all(&self, mut s: &[u8]) {
        while !s.is_empty() {
            match self.try_write_slice(s) {
                Ok(n) if n > 0 => s = &s[n..],
                _ => core::hint::spin_loop(),
            }
        }
    }

    fn flush(&self) {
        ByteSink::flush(self);
    }
}

// SAFETY: UartPl011 содержит только MmioBound (Sync), операции через volatile.
unsafe impl Send for UartPl011 {}

struct UartPl011Driver {
    address: MmioAddress,
}

impl Driver for UartPl011Driver {
    fn run(&mut self, services: &mut BootServices) -> Result<(), DriverRunError> {
        let mmio = services
            .require_mmio()
            .map_err(DriverRunError::from_boot_services_error)?;

        let bound = mmio
            .map_mmio(
                self.address,
                Owners::<UncachedPermission>::kernel(UncachedPermission::writable()),
            )
            .map_err(|e: drivers_common::services::mmio::MmioMapError| {
                DriverRunError::Fatal(alloc::format!("{e}"))
            })?;

        let uart = UartPl011::new(bound);
        uart.mmio.write_reg(CR, CR_UARTEN | CR_TXE | CR_RXE);

        services
            .set_console(Arc::new(uart))
            .map_err(DriverRunError::from_boot_services_error)
    }
}

struct UartPl011Factory {
    address: MmioAddress,
}

impl DriverFactory for UartPl011Factory {
    fn create(&self) -> Result<Box<dyn Driver>, alloc::string::String> {
        Ok(Box::new(UartPl011Driver {
            address: self.address,
        }))
    }
}

pub fn uart_pl011_probe(context: &mut FdtProbeContext<'_>) -> ProbeResult {
    drivers_common_aarch64::require_compatible(context.node(), &["arm,pl011"])?;

    if !context.is_console() {
        return Err(ProbeError::Unsupported("not the designated console"));
    }

    let address = context
        .get_mmio_address(REG_UART_INDEX)
        .ok_or(ProbeError::MissingProperty("base address"))?;

    Ok(Box::new(UartPl011Factory { address }))
}

register_driver!(UART_PL011_DRIVER, probe = uart_pl011_probe);
