//! Драйвер UART PL011 (ARM PrimeCell).

use crate::register_driver;
use alloc::boxed::Box;
use alloc::sync::Arc;
use core::hint::spin_loop;
use drivers_common::probe::{ProbeError, ProbeResult};
use drivers_common::services::mmio::{MmioAddress, MmioBound, MmioService};
use drivers_common::services::console::ConsoleService;
use drivers_common::{
    CapabilityStoreExt, CapabilityStoreMut, CapabilityStoreMutExt, DeviceMemoryPermission, Driver,
    DriverFactory, DriverRunError, Owners,
};
use drivers_common_aarch64::{FdtProbeContext, ProbeContextExt};
use io::byte_sink::{ByteSink, Pending};
use io::mmio::Reg;
use io::writer::Writer;
use util::crlf::Crlf;

/// Регистр данных.
const DR: Reg<u32> = Reg::new(0x00);
/// Регистр флагов.
const FR: Reg<u32> = Reg::new(0x18);
/// Регистр управления.
const CR: Reg<u32> = Reg::new(0x30);

/// TX FIFO заполнен.
const FR_TXFF: u32 = 1 << 5;
/// UART занят передачей.
const FR_BUSY: u32 = 1 << 3;

/// Включение UART.
const CR_UARTEN: u32 = 1 << 0;
/// Включение передатчика.
const CR_TXE: u32 = 1 << 8;
/// Включение приёмника.
const CR_RXE: u32 = 1 << 9;

/// Индекс записи reg в DeviceTree.
const REG_UART_INDEX: usize = 0;

/// Драйвер UART PL011.
pub struct UartPl011 {
    /// MMIO-доступ к регистрам.
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

        self.mmio.write_reg(DR, b as u32);
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

            self.mmio.write_reg(DR, byte as u32);
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

impl ConsoleService for UartPl011 {
    fn writer(&self) -> &(dyn Writer + Sync) {
        self
    }
}

// SAFETY: UartPl011 содержит только MmioBound (Sync), операции через volatile.
unsafe impl Send for UartPl011 {}

struct UartPl011Driver {
    address: MmioAddress,
}

impl Driver for UartPl011Driver {
    fn run(&mut self, caps: &mut dyn CapabilityStoreMut) -> Result<(), DriverRunError> {
        let mmio = caps
            .require_service::<dyn MmioService>()
            .map_err(DriverRunError::from_capability_error)?;

        let bound = mmio
            .map_mmio(
                self.address,
                Owners::<DeviceMemoryPermission>::kernel(DeviceMemoryPermission::writable()),
            )
            .map_err(|e: drivers_common::services::mmio::MmioMapError| {
                DriverRunError::Fatal(alloc::format!("{}", e))
            })?;

        let uart = UartPl011::new(bound);
        uart.mmio.write_reg(CR, CR_UARTEN | CR_TXE | CR_RXE);

        caps.provide_service::<dyn ConsoleService>(Arc::new(uart))
            .map_err(|e| DriverRunError::Fatal(alloc::format!("{}", e)))
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

    let address = context
        .get_mmio_address(REG_UART_INDEX)
        .ok_or(ProbeError::MissingProperty("base address"))?;

    Ok(Box::new(UartPl011Factory { address }))
}

register_driver!(UART_PL011_DRIVER, probe = uart_pl011_probe);
