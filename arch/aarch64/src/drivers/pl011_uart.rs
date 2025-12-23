use crate::drivers::commons::ProbeContextExt;
use alloc::boxed::Box;
use core::hint::spin_loop;
use io::byte_sink::{ByteSink, WouldBlock};
use io::mmio::{Mmio, Reg};
use io::writer::{BlockingWriter, Writer};
use kernel::driver::early::{EarlyDriver, EarlyDriverContext, ProbeContext};
use kernel::driver::probe::ProbeResult;
use kernel::driver::register_early_driver;
use util::crlf::Crlf;

const DR: Reg<u32> = Reg::new(0x00); // Data Reg
const FR: Reg<u32> = Reg::new(0x18); // Flag Reg

// Биты регистра FR
const FR_TXFF: u32 = 1 << 5; // Передающий FIFO заполнен
const FR_BUSY: u32 = 1 << 3; // UART занят передачей

const REG_UART_INDEX: usize = 0;
const REG_UART_SIZE_INDEX: usize = 1;

pub struct UartPl011 {
    mmio: Mmio,
    base: usize,
}

// Драйвер UART PL011 (ARM PrimeCell)
impl UartPl011 {
    pub(crate) const fn new(base: usize) -> Self {
        Self {
            mmio: Mmio::new(base),
            base,
        }
    }
}

impl ByteSink for UartPl011 {
    fn try_write(&self, b: u8) -> Result<(), WouldBlock> {
        // Выходим, если очередь не пуста
        if (self.mmio.read_reg(FR) & FR_TXFF) != 0 {
            return Err(WouldBlock);
        }

        self.mmio.write_reg(DR, b as u32);
        Ok(())
    }

    fn try_write_slice(&self, buf: &[u8]) -> Result<usize, WouldBlock> {
        if buf.is_empty() {
            return Ok(0);
        }

        let mut crlf = Crlf::new(buf);
        let mut consumed = 0usize;

        while let Some(byte) = crlf.next() {
            if (self.mmio.read_reg(FR) & FR_TXFF) != 0 {
                if consumed == 0 {
                    return Err(WouldBlock);
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
        context.request_mmio(self.base, 4096);

        Ok(())
    }

    fn output(&self) -> Option<Box<dyn Writer + Sync + '_>> {
        Some(Box::new(BlockingWriter::new(self)))
    }
}

fn uart_pl011_probe(context: &mut ProbeContext<'_>) -> ProbeResult<Box<dyn EarlyDriver>> {
    let base = context.reg_offset::<REG_UART_SIZE_INDEX>(REG_UART_INDEX);

    Ok(Box::new(UartPl011::new(base)))
}

register_early_driver!(
    UART_PL011_EARLY,
    compatible = &["arm,pl011", "arm,primecell"],
    probe = uart_pl011_probe
);
