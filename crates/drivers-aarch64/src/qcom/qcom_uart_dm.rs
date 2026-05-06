//! Драйвер Qualcomm UART DM (Data Mover).

use alloc::{boxed::Box, sync::Arc};

use drivers_common::{
    CapabilityStoreExt, CapabilityStoreMut, CapabilityStoreMutExt, DeviceMemoryPermission, Driver,
    DriverFactory, DriverRunError, Owners,
    probe::{ProbeError, ProbeResult},
    services::{
        console::ConsoleService,
        mmio::{MmioAddress, MmioBound, MmioService},
    },
};
use drivers_common_aarch64::{FdtProbeContext, ProbeContextExt};
use io::{
    byte_sink::{ByteSink, Pending},
    mmio::Reg,
    writer::Writer,
};
use util::crlf::Crlf;

use crate::register_driver;

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
    mmio: MmioBound,
}

impl UartDm {
    pub fn new(mmio: MmioBound) -> Self {
        Self { mmio }
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

impl Writer for UartDm {
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

// SAFETY: UartDm содержит только MmioBound (Sync), операции через volatile.
unsafe impl Send for UartDm {}

struct UartDmDriver {
    address: MmioAddress,
}

impl Driver for UartDmDriver {
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
                DriverRunError::Fatal(alloc::format!("{e}"))
            })?;

        let uart = UartDm::new(bound);

        caps.provide_service::<dyn ConsoleService>(Arc::new(uart))
            .map_err(|e| DriverRunError::Fatal(alloc::format!("{e}")))
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
