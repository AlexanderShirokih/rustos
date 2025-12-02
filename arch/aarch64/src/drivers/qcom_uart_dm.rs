use crate::drivers::commons::ProbeContextExt;
use alloc::boxed::Box;
use io::byte_sink::{ByteSink, WouldBlock};
use io::mmio::{Mmio, Reg};
use io::writer::BlockingWriter;
use kernel::driver::early::{EarlyDriverHandle, ProbeContext};
use kernel::driver::probe::ProbeResult;
use kernel::driver::register_early_driver;
use util::crlf::Crlf;

// --- UARTDM v1.4 регистры и биты ---
const NCF_TX: Reg<u32> = Reg::new(0x040); // number of chars for TX
const SR: Reg<u32> = Reg::new(0x0A4); // status
const CR: Reg<u32> = Reg::new(0x0A8); // command/enable
const TF: Reg<u32> = Reg::new(0x100); // TX FIFO (32-bit writes)

const SR_TXRDY: u32 = 1 << 2;
const SR_TXEMT: u32 = 1 << 3;
const CMD_CLEAR_TX_READY: u32 = 0x300; // kick NCF_TX

const REG_UART_INDEX: usize = 0;
const REG_UART_SIZE: usize = 1;

pub struct UartDm {
    mmio: Mmio,
}

impl UartDm {
    pub(crate) const fn new(base: usize) -> Self {
        Self {
            mmio: Mmio::new(base),
        }
    }
}

impl ByteSink for UartDm {
    fn try_write(&self, b: u8) -> Result<(), WouldBlock> {
        match self.try_write_slice(core::slice::from_ref(&b)) {
            Ok(1) => Ok(()),
            _ => Err(WouldBlock),
        }
    }

    fn try_write_slice(&self, buf: &[u8]) -> Result<usize, WouldBlock> {
        if buf.is_empty() {
            return Ok(0);
        }

        // Очередь должна быть пуста
        if (self.mmio.read_reg(SR) & SR_TXEMT) == 0 {
            return Err(WouldBlock);
        }

        // Сформируем слово (LF -> CRLF, до 4 байт)
        let mut crlf = Crlf::new(buf);
        let (word, out_len, in_consumed) = crlf.pack_u32_le();
        if out_len == 0 {
            return Ok(0);
        }

        // Задаём размер выпуска и сбрасываем TX_READY (старт передачи после записи в TF)
        self.mmio.write_reg(NCF_TX, out_len as u32);
        self.mmio.write_reg(CR, CMD_CLEAR_TX_READY);

        // Проверяем готовность к приему слова
        if (self.mmio.read_reg(SR) & SR_TXRDY) == 0 {
            return Err(WouldBlock);
        }

        self.mmio.write_reg(TF, word);

        Ok(in_consumed)
    }

    fn flush(&self) {
        // Ждём полного опустошения передатчика
        while (self.mmio.read_reg(SR) & SR_TXEMT) == 0 {
            core::hint::spin_loop();
        }
    }
}

fn uart_dm_probe(context: &ProbeContext) -> ProbeResult<EarlyDriverHandle> {
    let uart = UartDm::new(context.reg_offset::<REG_UART_SIZE>(REG_UART_INDEX));
    let writer = BlockingWriter::new(uart);

    Ok(EarlyDriverHandle::Writer(Box::new(writer)))
}

register_early_driver!(
    UART_DM_EARLY,
    compatible = &["qcom,msm-uartdm", "qcom,msm-hsuart"],
    probe = uart_dm_probe
);
