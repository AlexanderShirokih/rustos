use kernel_core::byte_sink::{ByteSink, WouldBlock};

// UARTDM v1.4 offsets (байтовые) и маски:
const NCF_TX: usize = 0x040; // "number of chars for TX"
const SR: usize = 0x0A4; // status
const CR: usize = 0x0A8; // command / enable
const TF: usize = 0x100; // TX FIFO (32-битные записи)

// Биты/команды (см. msm_serial_hs_hwreg.h):
const SR_TXRDY: u32 = 1 << 2;
const SR_TXEMT: u32 = 1 << 3;

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

    /// Преобразует входной буфер, заменяя \n на \r\n
    /// Возвращает (out_buf, out_len, in_consumed)
    #[inline]
    fn convert_newlines(buf: &[u8]) -> ([u8; 4], usize, usize) {
        let mut out_buf = [0u8; 4];
        let mut out_len = 0;
        let mut in_pos = 0;

        while in_pos < buf.len() && out_len < 4 {
            let b = buf[in_pos];
            if b == b'\n' {
                // Добавляем \r перед \n
                if out_len < 3 {
                    out_buf[out_len] = b'\r';
                    out_len += 1;
                    out_buf[out_len] = b'\n';
                    out_len += 1;
                    in_pos += 1;
                } else if out_len < 4 {
                    // Влезет только \r, \n в следующий раз
                    out_buf[out_len] = b'\r';
                    out_len += 1;
                    break;
                } else {
                    break;
                }
            } else {
                out_buf[out_len] = b;
                out_len += 1;
                in_pos += 1;
            }
        }

        (out_buf, out_len, in_pos)
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

        // Формируем буфер с заменой \n на \r\n
        let (out_buf, out_len, in_consumed) = Self::convert_newlines(buf);

        if out_len == 0 {
            return Ok(0);
        }

        let mut word = 0u32;
        for i in 0..out_len {
            word |= (out_buf[i] as u32) << (i * 8);
        }

        // Задаём размер выпуска и сбрасываем TX_READY (старт передачи после записи в TF)
        self.w32(NCF_TX, out_len as u32);
        self.w32(CR, CMD_CLEAR_TX_READY);

        // Ждём место в FIFO и кладём слово
        if (self.r32(SR) & SR_TXRDY) == 0 {
            return Err(WouldBlock);
        }
        self.w32(TF, word);

        Ok(in_consumed)
    }

    #[inline(always)]
    fn flush(&self) {
        // Ждём полного опустошения передатчика
        while (self.r32(SR) & SR_TXEMT) == 0 {
            core::hint::spin_loop();
        }
    }
}
