//! Драйвер Qualcomm UART DM (Data Mover).

use alloc::{boxed::Box, sync::Arc};
use core::{
    hint::spin_loop,
    sync::atomic::{AtomicBool, Ordering},
};

use drivers_common::{
    BootServices, DeviceMemoryPermission, Driver, DriverFactory, DriverRunError, Owners,
    probe::{ProbeError, ProbeResult},
    services::mmio::{MmioAddress, MmioBound},
};
use drivers_common_aarch64::{FdtProbeContext, ProbeContextExt};
use io::{
    byte_sink::{ByteSink, Pending},
    mmio::Reg,
    writer::Writer,
};
use util::crlf::Crlf;

use crate::register_driver;

// Регистры UART DM. TF принимает слово целиком, 32-битной записью.

const NCF_TX: Reg<u32> = Reg::new(0x040);
const SR: Reg<u32> = Reg::new(0x0A4);
const CR: Reg<u32> = Reg::new(0x0A8);
const TF: Reg<u32> = Reg::new(0x100);

// Биты SR и команды CR

const SR_TXRDY: u32 = 1 << 2;
const SR_TXEMT: u32 = 1 << 3;
const CMD_CLEAR_TX_READY: u32 = 0x300;

const REG_UART_INDEX: usize = 0;

const TX_SEQUENCE_SPIN_LIMIT: usize = 1_000_000;

/// Драйвер Qualcomm UART DM.
pub struct UartDm {
    mmio: MmioBound,
    tx_sequence_claimed: AtomicBool,
}

struct TxSequenceClaim<'a>(&'a UartDm);

impl UartDm {
    pub fn new(mmio: MmioBound) -> Self {
        Self {
            mmio,
            tx_sequence_claimed: AtomicBool::new(false),
        }
    }

    fn claim_tx_sequence(&self) -> TxSequenceClaim<'_> {
        // Ожидание без предела: таймаут не доказывает смерть владельца.
        loop {
            if let Some(claim) = self.try_claim_tx_sequence() {
                return claim;
            }
            spin_loop();
        }
    }

    fn claim_tx_sequence_for_panic(&self) -> TxSequenceClaim<'_> {
        for _ in 0..TX_SEQUENCE_SPIN_LIMIT {
            if let Some(claim) = self.try_claim_tx_sequence() {
                return claim;
            }
            spin_loop();
        }

        // Захват переходит силой, и вызывающий гарантирует, что вытесняемый
        // владелец не возобновится. Брошенная последовательность не мешает
        // свежей: слово, не поданное в TF, передачу не начинает, а поданное
        // дренируется железом и наблюдается через SR_TXEMT.
        self.tx_sequence_claimed.swap(true, Ordering::Acquire);
        TxSequenceClaim(self)
    }

    fn try_claim_tx_sequence(&self) -> Option<TxSequenceClaim<'_>> {
        self.tx_sequence_claimed
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
            .then(|| TxSequenceClaim(self))
    }
}

impl TxSequenceClaim<'_> {
    fn try_feed_word(&self, buf: &[u8]) -> Result<usize, Pending> {
        // NCF_TX, CR и TF образуют одну последовательность передачи и пишутся
        // только под захваченным `TxSequenceClaim`.
        let mmio = &self.0.mmio;

        // Очередь должна быть пуста
        if (mmio.read_reg(SR) & SR_TXEMT) == 0 {
            return Err(Pending);
        }

        // Формирование слова (LF -> CRLF, до 4 байт)
        let mut crlf = Crlf::new(buf);
        let (word, out_len, in_consumed) = crlf.pack_u32_le();
        if out_len == 0 {
            return Ok(0);
        }

        // Установка размера выпуска и сброс TX_READY (старт передачи после записи в TF)
        mmio.write_reg(NCF_TX, out_len as u32);
        mmio.write_reg(CR, CMD_CLEAR_TX_READY);

        // Проверка готовности к приему слова
        if (mmio.read_reg(SR) & SR_TXRDY) == 0 {
            return Err(Pending);
        }

        mmio.write_reg(TF, word);

        Ok(in_consumed)
    }
}

impl Drop for TxSequenceClaim<'_> {
    fn drop(&mut self) {
        self.0.tx_sequence_claimed.store(false, Ordering::Release);
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

        self.claim_tx_sequence().try_feed_word(buf)
    }

    fn flush(&self) {
        // Ожидание полного опустошения передатчика
        while (self.mmio.read_reg(SR) & SR_TXEMT) == 0 {
            spin_loop();
        }
    }
}

impl Writer for UartDm {
    fn write_all(&self, mut s: &[u8]) {
        while !s.is_empty() {
            match self.try_write_slice(s) {
                Ok(n) if n > 0 => s = &s[n..],
                _ => spin_loop(),
            }
        }
    }

    fn write_all_panic(&self, mut s: &[u8]) {
        if s.is_empty() {
            return;
        }

        // Один захват на всё сообщение: конкуренты пути паники мертвы или
        // остановлены, отдавать им последовательность между словами незачем.
        let claim = self.claim_tx_sequence_for_panic();
        while !s.is_empty() {
            match claim.try_feed_word(s) {
                Ok(n) if n > 0 => s = &s[n..],
                _ => spin_loop(),
            }
        }
    }

    fn flush(&self) {
        ByteSink::flush(self);
    }
}

// SAFETY: UartDm содержит MmioBound (доступ через volatile) и AtomicBool -
// оба корректны при передаче между потоками.
unsafe impl Send for UartDm {}

struct UartDmDriver {
    address: MmioAddress,
}

impl Driver for UartDmDriver {
    fn run(&mut self, services: &mut BootServices) -> Result<(), DriverRunError> {
        let mmio = services
            .require_mmio()
            .map_err(DriverRunError::from_boot_services_error)?;

        let bound = mmio
            .map_mmio(
                self.address,
                Owners::<DeviceMemoryPermission>::kernel(DeviceMemoryPermission::writable()),
            )
            .map_err(|e: drivers_common::services::mmio::MmioMapError| {
                DriverRunError::Fatal(alloc::format!("{e}"))
            })?;

        let uart = UartDm::new(bound);

        services
            .set_console(Arc::new(uart))
            .map_err(DriverRunError::from_boot_services_error)
    }
}

struct UartDmFactory {
    address: MmioAddress,
}

impl DriverFactory for UartDmFactory {
    fn create(&self) -> Result<Box<dyn Driver>, alloc::string::String> {
        Ok(Box::new(UartDmDriver {
            address: self.address,
        }))
    }
}

pub fn uart_dm_probe(context: &mut FdtProbeContext<'_>) -> ProbeResult {
    drivers_common_aarch64::require_compatible(
        context.node(),
        &["qcom,msm-uartdm", "qcom,msm-hsuart"],
    )?;

    let address = context
        .get_mmio_address(REG_UART_INDEX)
        .ok_or(ProbeError::MissingProperty("base addr"))?;

    Ok(Box::new(UartDmFactory { address }))
}

register_driver!(UART_DM_DRIVER, probe = uart_dm_probe);
