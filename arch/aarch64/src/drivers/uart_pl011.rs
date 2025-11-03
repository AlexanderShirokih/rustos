use core::hint::spin_loop;
use kernel_core::io::byte_sink::{ByteSink, WouldBlock};
use kernel_core::io::mmio::{Mmio, Register};
use util::crlf::Crlf;

// Драйвер UART PL011 (ARM PrimeCell)
// Используется QEMU virt: базовый адрес по умолчанию 0x09000000

const DR: Register<u32> = Register::new(0x00); // Data Register
const FR: Register<u32> = Register::new(0x18); // Flag Register

// Биты регистра FR
const FR_TXFF: u32 = 1 << 5; // Передающий FIFO заполнен
const FR_BUSY: u32 = 1 << 3; // UART занят передачей

pub struct UartPl011 {
    mmio: Mmio,
}

impl UartPl011 {
    pub(crate) const fn new(base: usize) -> Self {
        Self {
            mmio: Mmio::new(base),
        }
    }
}

impl ByteSink for UartPl011 {
    #[inline(always)]
    fn try_write(&self, b: u8) -> Result<(), WouldBlock> {
        // Выходим, если очередь не пуста
        if (self.mmio.read_reg(FR) & FR_TXFF) != 0 {
            return Err(WouldBlock);
        }

        self.mmio.write_reg(DR, b as u32);
        Ok(())
    }

    #[inline(always)]
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

    #[inline(always)]
    fn flush(&self) {
        while (self.mmio.read_reg(FR) & FR_BUSY) != 0 {
            spin_loop();
        }
    }
}
