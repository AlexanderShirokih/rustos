//! Драйвер UART PL011 (ARM PrimeCell).

use alloc::boxed::Box;
use core::hint::spin_loop;
use drivers_common::{EarlyDriver, EarlyDriverContext, ProbeResult};
use drivers_common_aarch64::FdtProbeContext;
use drivers_common_aarch64::ProbeContextExt;
use drivers_common_aarch64::register_early_driver;
use io::byte_sink::{ByteSink, Pending};
use io::mmio::{Mmio, Reg};
use io::writer::{BlockingWriter, Writer};
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
/// Индекс размера в записи reg.
const REG_UART_SIZE_INDEX: usize = 1;

/// Драйвер UART PL011.
pub struct UartPl011 {
    /// MMIO-доступ к регистрам.
    mmio: Mmio,
    /// Базовый адрес регистров.
    base: usize,
}

impl UartPl011 {
    pub const fn new(base: usize) -> Self {
        Self {
            mmio: Mmio::new(base),
            base,
        }
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

impl EarlyDriver for UartPl011 {
    fn init(&self, context: &mut EarlyDriverContext) -> Result<(), &'static str> {
        context.map_mmio(self.base, 4096);

        // Включение UART и передатчика
        self.mmio.write_reg(CR, CR_UARTEN | CR_TXE | CR_RXE);

        Ok(())
    }

    fn output(&self) -> Option<Box<dyn Writer + Sync + '_>> {
        Some(Box::new(BlockingWriter::new(self)))
    }
}

pub fn uart_pl011_probe(context: &mut FdtProbeContext<'_>) -> ProbeResult<Box<dyn EarlyDriver>> {
    drivers_common_aarch64::require_compatible(context.node(), &["arm,pl011"])?;

    let base = context.reg_offset::<REG_UART_SIZE_INDEX>(REG_UART_INDEX);

    Ok(Box::new(UartPl011::new(base)))
}

register_early_driver!(UART_PL011_EARLY, probe = uart_pl011_probe);
