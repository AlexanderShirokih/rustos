//! Драйвер Qualcomm UART DM (Data Mover).

use crate::register_early_driver;
use alloc::boxed::Box;
use drivers_common::probe::ProbeError;
use drivers_common::services::mmio::MmioAddress;
use drivers_common::{EarlyDriver, EarlyDriverContext, EarlyProbeResult};
use drivers_common_aarch64::FdtProbeContext;
use drivers_common_aarch64::ProbeContextExt;
use io::byte_sink::{ByteSink, Pending};
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

/// Драйвер Qualcomm UART DM.
pub struct UartDm {
    /// MMIO-доступ к регистрам.
    mmio: Mmio,

    /// Базовый адрес регистров.
    address: MmioAddress,
}

impl UartDm {
    pub const fn new(address: MmioAddress) -> Self {
        Self {
            mmio: Mmio::new(address.base()),
            address,
        }
    }
}

impl EarlyDriver for UartDm {
    fn init(&self, context: &mut EarlyDriverContext) -> Result<(), &'static str> {
        context.map_mmio(self.address);

        Ok(())
    }

    fn output(&self) -> Option<Box<dyn Writer + Sync + '_>> {
        let writer = BlockingWriter::new(self);

        Some(Box::new(writer))
    }
}

impl ByteSink for UartDm {
    fn try_write(&self, b: u8) -> Result<(), Pending> {
        match self.try_write_slice(core::slice::from_ref(&b)) {
            Ok(1) => Ok(()),
            _ => Err(Pending),
        }
    }

    fn try_write_slice(&self, buf: &[u8]) -> Result<usize, Pending> {
        if buf.is_empty() {
            return Ok(0);
        }

        // Очередь должна быть пуста
        if (self.mmio.read_reg(SR) & SR_TXEMT) == 0 {
            return Err(Pending);
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
            return Err(Pending);
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

pub fn uart_dm_probe(context: &mut FdtProbeContext<'_>) -> EarlyProbeResult {
    drivers_common_aarch64::require_compatible(
        context.node(),
        &["qcom,msm-uartdm", "qcom,msm-hsuart"],
    )?;

    let address = context
        .get_mmio_address(REG_UART_INDEX)
        .ok_or(ProbeError::MissingProperty("base addr"))?;

    Ok(Box::new(UartDm::new(address)))
}

register_early_driver!(UART_DM_EARLY, probe = uart_dm_probe);
