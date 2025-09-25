use core::arch::asm;
use kernel_core::byte_sink::{ByteSink, WouldBlock};
use kernel_core::device::device::Device;

// UARTDM v1.4 offsets (байтовые) и маски:
const TFWR: usize = 0x01C;
const RFWR: usize = 0x020;
const NCF_TX: usize = 0x040; // "number of chars for TX"
const SR: usize = 0x0A4; // status
const CR: usize = 0x0A8; // command / enable
const IMR: usize = 0x0B0; // interrupt mask
const TF: usize = 0x100; // TX FIFO (32-битные записи)

// Биты/команды (см. msm_serial_hs_hwreg.h):
const SR_TXRDY: u32 = 1 << 2;
const SR_TXEMT: u32 = 1 << 3;

const CR_RX_EN: u32 = 1 << 0;
const CR_TX_EN: u32 = 1 << 2;

// Команды в поле [8:4] CR:
const CMD_RESET_RX: u32 = 0x10;
const CMD_RESET_TX: u32 = 0x20;
const CMD_RESET_ERR: u32 = 0x30;
const CMD_RESET_BREAK_INT: u32 = 0x40;
const CMD_CLEAR_TX_READY: u32 = 0x300; // запускает передачу NCF_TX символов

pub struct UartMmio {
    base: usize,
}

impl UartMmio {
    pub(crate) fn new(base: usize) -> Self {
        Self { base }
    }

    #[inline(always)]
    fn r32(&self, off: usize) -> u32 {
        unsafe { core::ptr::read_volatile((self.base + off) as *const u32) }
    }
    #[inline(always)]
    fn w32(&self, off: usize, v: u32) {
        unsafe { core::ptr::write_volatile((self.base + off) as *mut u32, v) }
    }
}

impl ByteSink for UartMmio {
    #[inline(always)]
    fn try_write(&self, b: u8) -> Result<(), WouldBlock> {
        match self.try_write_slice(core::slice::from_ref(&b)) {
            Ok(1) => Ok(()),
            _ => Err(WouldBlock),
        }
    }

    #[inline(always)]
    fn try_write_slice(&self, buf: &[u8]) -> Result<usize, WouldBlock> {
        if buf.is_empty() {
            return Ok(0);
        }

        // FIFO должен быть пуст
        if (self.r32(SR) & SR_TXEMT) == 0 {
            return Err(WouldBlock);
        }

        // Задаём размер выпуска и сбрасываем TX_READY
        let n = core::cmp::min(4, buf.len());
        self.w32(NCF_TX, n as u32);
        self.w32(CR, CMD_CLEAR_TX_READY);

        // Ждём место в FIFO
        if (self.r32(SR) & SR_TXRDY) == 0 {
            return Err(WouldBlock);
        }

        let mut word = 0u32;
        for i in 0..n {
            word |= (buf[i] as u32) << (i * 8);
        }
        self.w32(TF, word);

        Ok(n)
    }

    #[inline(always)]
    fn flush(&self) {
        // Ждём полного опустошения передатчика
        while (self.r32(SR) & SR_TXRDY) == 0 {
            core::hint::spin_loop();
        }
    }
}

impl Device for UartMmio {
    fn init(&self) {
        unsafe {
            // Глушим IRQ (работаем busy-wait)
            self.w32(IMR, 0);

            // Сбросим каналы/флаги ошибок
            self.w32(CR, CMD_RESET_RX);
            self.w32(CR, CMD_RESET_TX);
            self.w32(CR, CMD_RESET_ERR);
            self.w32(CR, CMD_RESET_BREAK_INT);

            self.w32(TFWR, 1);
            self.w32(RFWR, 1);

            // Включим RX/TX (битовые enable в CR)
            self.w32(CR, CR_RX_EN | CR_TX_EN);

            asm!("dsb sy; isb", options(nostack, preserves_flags));
        }
    }

    fn uninit(&self) {
        // выключить регистры UART?
    }
}
