//! Драйвер Qualcomm UART DM (Data Mover).

use alloc::boxed::Box;
use drivers_common::{EarlyDriver, EarlyDriverContext, ProbeResult};
use drivers_common_aarch64::FdtProbeContext;
use drivers_common_aarch64::ProbeContextExt;
use drivers_common_aarch64::register_early_driver;
use io::byte_sink::{ByteSink, WouldBlock};
use io::mmio::{Mmio, Reg};
use io::writer::{BlockingWriter, Writer};
use util::crlf::Crlf;

/// Количество символов для передачи.
const NCF_TX: Reg<u32> = Reg::new(0x040);
/// Регистр статуса.
const SR: Reg<u32> = Reg::new(0x0A4);
/// Регистр команд.
const CR: Reg<u32> = Reg::new(0x0A8);
/// TX FIFO (32-битная запись).
const TF: Reg<u32> = Reg::new(0x100);

/// TX готов к приёму данных.
const SR_TXRDY: u32 = 1 << 2;
/// TX FIFO пуст.
const SR_TXEMT: u32 = 1 << 3;
/// Команда сброса TX_READY.
const CMD_CLEAR_TX_READY: u32 = 0x300;

/// Индекс записи reg в DeviceTree.
const REG_UART_INDEX: usize = 0;
/// Индекс размера в записи reg.
const REG_UART_SIZE_INDEX: usize = 1;

/// Драйвер Qualcomm UART DM.
pub struct UartDm {
    /// MMIO-доступ к регистрам.
    mmio: Mmio,
    /// Базовый адрес регистров.
    base: usize,
}

impl UartDm {
    pub const fn new(base: usize) -> Self {
        Self {
            mmio: Mmio::new(base),
            base,
        }
    }
}

impl EarlyDriver for UartDm {
    fn init(&self, context: &mut EarlyDriverContext) -> Result<(), &'static str> {
        context.map_mmio(self.base, 4096);

        Ok(())
    }

    fn output(&self) -> Option<Box<dyn Writer + Sync + '_>> {
        let writer = BlockingWriter::new(self);

        Some(Box::new(writer))
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

        // Формирование слова (LF -> CRLF, до 4 байт)
        let mut crlf = Crlf::new(buf);
        let (word, out_len, in_consumed) = crlf.pack_u32_le();
        if out_len == 0 {
            return Ok(0);
        }

        // Установка размера выпуска и сброс TX_READY (старт передачи после записи в TF)
        self.mmio.write_reg(NCF_TX, out_len as u32);
        self.mmio.write_reg(CR, CMD_CLEAR_TX_READY);

        // Проверка готовности к приему слова
        if (self.mmio.read_reg(SR) & SR_TXRDY) == 0 {
            return Err(WouldBlock);
        }

        self.mmio.write_reg(TF, word);

        Ok(in_consumed)
    }

    fn flush(&self) {
        // Ожидание полного опустошения передатчика
        while (self.mmio.read_reg(SR) & SR_TXEMT) == 0 {
            core::hint::spin_loop();
        }
    }
}

pub fn uart_dm_probe(context: &mut FdtProbeContext<'_>) -> ProbeResult<Box<dyn EarlyDriver>> {
    drivers_common_aarch64::require_compatible(
        context.node(),
        &["qcom,msm-uartdm", "qcom,msm-hsuart"],
    )?;

    let base = context.reg_offset::<REG_UART_SIZE_INDEX>(REG_UART_INDEX);

    Ok(Box::new(UartDm::new(base)))
}

register_early_driver!(UART_DM_EARLY, probe = uart_dm_probe);
