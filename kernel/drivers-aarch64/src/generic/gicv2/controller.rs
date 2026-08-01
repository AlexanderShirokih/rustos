//! Аппаратный контроллер GICv2: Distributor и CPU Interface.

use alloc::{collections::BTreeMap, sync::Arc};

use drivers_common::services::{
    interrupts::{
        CpuInterfaceError, CpuMask, IpiError, IpiTarget, IrqHandler, IrqNumber, IrqPriority,
        TriggerType,
    },
    mmio::MmioBound,
};
use drivers_common_aarch64::ipi_affinity::IpiAffinity;
use klog::debug;

use super::regs::{
    GICC_BPR, GICC_CTLR, GICC_EOIR, GICC_IAR, GICC_PMR, GICD_CTLR, GICD_ICENABLER, GICD_ICFGR,
    GICD_ICPENDR, GICD_IPRIORITYR, GICD_ISENABLER, GICD_ITARGETSR, GICD_SGIR, GICD_TYPER,
    ITLinesNumber, IrqType, PRIORITY_MASK_ALL, bit_offset,
};
use crate::read_sysreg;

const fn sgir_value(irq: IrqNumber, target_list: u8) -> u32 {
    ((target_list as u32) << 16) | ((irq.raw() & 0xF) as u32)
}

const _: () = assert!(sgir_value(IrqNumber::new(0), 0b0000_0010) == 0x0002_0000);
const _: () = assert!(sgir_value(IrqNumber::new(15), 0b1000_0000) == 0x0080_000F);

/// Runtime-объект контроллера прерываний GICv2.
pub(super) struct Gicv2Controller {
    pub(super) distributor: MmioBound,
    pub(super) cpu_interface: MmioBound,
    pub(super) interrupt_lines: ITLinesNumber,
    pub(super) handlers: BTreeMap<IrqNumber, BoundIrq>,
    /// Маска CPU interface каждого CPU по его `Aff0`. Заполняется каждым CPU
    /// на своей инициализации: соответствие объявляет железо, а не топология.
    interface_masks: BTreeMap<u8, u8>,
}

/// Привязанная линия: обработчик и параметры её настройки.
///
/// Регистры SGI и PPI банкированы по CPU: настройка, сделанная одним CPU,
/// другому не видна, и на его per-CPU инициализации она повторяется отсюда.
pub(super) struct BoundIrq {
    pub(super) handler: Arc<dyn IrqHandler>,
    pub(super) priority: IrqPriority,
    pub(super) trigger: Option<TriggerType>,
}

// SAFETY: Gicv2Controller содержит только Mmio (который Sync).
// Все операции с Mmio выполняются через volatile, что безопасно для многопоточности.
unsafe impl Send for Gicv2Controller {}

impl Gicv2Controller {
    pub(super) fn new(distributor: MmioBound, cpu_interface: MmioBound) -> Self {
        Self {
            handlers: BTreeMap::new(),
            interface_masks: BTreeMap::new(),
            interrupt_lines: Self::get_interrupts_lines(&distributor),
            distributor,
            cpu_interface,
        }
    }

    pub(super) fn init(&mut self) -> Result<(), CpuInterfaceError> {
        Self::set_irq_mask();

        debug!("Enabling GICv2");
        debug!("  GICD @ {}", self.distributor.base());
        debug!("  GICC @ {}", self.cpu_interface.base());

        self.init_distributor();
        self.init_this_cpu()
    }

    /// Настраивает CPU interface текущего CPU: его регистры банкированы по CPU.
    /// Distributor общий для всех CPU и здесь не участвует.
    ///
    /// Оставляет IRQ замаскированными на текущем CPU.
    pub(super) fn init_this_cpu(&mut self) -> Result<(), CpuInterfaceError> {
        Self::set_irq_mask();
        self.init_cpu_interface();
        self.reset_private_lines();
        self.learn_interface_mask()?;
        self.restore_private_lines();

        Ok(())
    }

    fn reset_private_lines(&self) {
        self.distributor.write_reg(GICD_ICENABLER, u32::MAX);
        self.distributor.write_reg(GICD_ICPENDR, u32::MAX);
        for reg in 0..8 {
            self.distributor
                .write_reg(GICD_IPRIORITYR.with_offset(reg * 4), PRIORITY_MASK_ALL);
        }
        self.distributor.write_reg(GICD_ICFGR.with_offset(4), 0u32);
    }

    fn learn_interface_mask(&mut self) -> Result<(), CpuInterfaceError> {
        // SAFETY: MPIDR_EL1 доступен на EL1 и только читается.
        let aff0 = IpiAffinity::from_mpidr(unsafe { read_sysreg!(mpidr_el1) }).aff0;
        let mask = (self.distributor.read_reg(GICD_ITARGETSR) & 0xFF) as u8;
        if mask == 0 {
            return Err(CpuInterfaceError::NotFound);
        }

        self.interface_masks.insert(aff0, mask);
        Ok(())
    }

    fn restore_private_lines(&self) {
        for (irq, bound) in &self.handlers {
            if !matches!(IrqType::from_irq_number(*irq), IrqType::Sgi | IrqType::Ppi) {
                continue;
            }

            self.set_priority(*irq, bound.priority);
            if let Some(trigger) = bound.trigger {
                self.set_config(*irq, trigger);
            }
            self.enable(*irq);
        }
    }

    /// Рассылает SGI процессору через `GICD_SGIR`.
    ///
    /// GICv2 адресует CPU маской его интерфейса, а не affinity; маску объявил
    /// сам CPU на своей инициализации, до неё адресовать его нечем.
    pub(super) fn send_sgi(&self, irq: IrqNumber, target: IpiTarget) -> Result<(), IpiError> {
        if !matches!(IrqType::from_irq_number(irq), IrqType::Sgi) {
            return Err(IpiError::NotAnIpi);
        }

        let affinity = IpiAffinity::from_target(target);
        if !affinity.is_single_cluster() {
            return Err(IpiError::UnreachableTarget);
        }
        let target_list = *self
            .interface_masks
            .get(&affinity.aff0)
            .ok_or(IpiError::UnreachableTarget)?;

        // Записи, ради которых прерывание посылается, публикуются до его
        // генерации.
        // SAFETY: `dmb ishst` - барьер без операндов и побочных эффектов.
        unsafe {
            core::arch::asm!("dmb ishst", options(nostack, preserves_flags));
        }
        self.distributor
            .write_reg(GICD_SGIR, sgir_value(irq, target_list));

        Ok(())
    }

    pub(super) fn enable_global() {
        Self::clear_irq_mask();
    }

    pub(super) fn disable_global() {
        Self::set_irq_mask();

        // self.cpu_interface.write_reg(GICC_CTLR, 0);
        // self.distributor.write_reg(GICD_CTLR, 0);
    }

    fn init_distributor(&self) {
        // Отключаем Distributor перед настройкой
        self.distributor.write_reg(GICD_CTLR, 0);

        // Сбрасываем прерывания, pending флаги и configuration
        self.reset_interrupt_lines();
        self.set_level_sensitive_mode();

        // Включаем Distributor (EnableGrp0)
        self.distributor.write_reg(GICD_CTLR, 1);
    }

    fn init_cpu_interface(&self) {
        // Устанавливаем маски приоритета
        self.cpu_interface.write_reg(GICC_PMR, PRIORITY_MASK_ALL);
        self.cpu_interface.write_reg(GICC_BPR, 0);

        // Включаем CPU interface (EnableGrp0)
        self.cpu_interface.write_reg(GICC_CTLR, 0x01);
    }

    fn get_interrupts_lines(distributor: &MmioBound) -> ITLinesNumber {
        let typer = distributor.read_reg(GICD_TYPER);
        let it_lines_number = typer & 0x1F;

        (it_lines_number + 1) as usize
    }

    fn reset_interrupt_lines(&self) {
        for bank in 0..self.interrupt_lines {
            let offset = bank * 4;

            // Отключаем все прерывания
            self.distributor
                .write_reg(GICD_ICENABLER.with_offset(offset), u32::MAX);

            // Сбрасываем pending флаги
            self.distributor
                .write_reg(GICD_ICPENDR.with_offset(offset), u32::MAX);
        }
    }

    fn set_level_sensitive_mode(&self) {
        let num_config_regs = self.interrupt_lines * 2;

        for i in 0..num_config_regs {
            self.distributor.write_reg(GICD_ICFGR.with_offset(i * 4), 0);
        }
    }

    fn clear_irq_mask() {
        // SAFETY: Очистка бита I в DAIF разрешает обработку IRQ на текущем CPU.
        unsafe {
            core::arch::asm!("msr daifclr, #0b0010", options(nostack, preserves_flags));
        }
    }

    fn set_irq_mask() {
        // SAFETY: Установка бита I в DAIF запрещает обработку IRQ на текущем CPU.
        unsafe {
            core::arch::asm!("msr daifset, #0b0010", options(nostack, preserves_flags));
        }
    }

    pub(super) fn enable(&self, irq: IrqNumber) {
        let (reg, bit) = bit_offset(irq, 1);
        self.distributor
            .write_reg(GICD_ISENABLER.with_offset(reg * 4), 1 << bit);
    }

    pub(super) fn disable(&self, irq: IrqNumber) {
        let (reg, bit) = bit_offset(irq, 1);

        self.distributor
            .write_reg(GICD_ICENABLER.with_offset(reg * 4), 1 << bit);
    }

    pub(super) fn set_priority(&self, irq: IrqNumber, priority: IrqPriority) {
        let (reg, bit) = bit_offset(irq, 8);

        let priority_reg = GICD_IPRIORITYR.with_offset(reg * 4);
        let mut val = self.distributor.read_reg(priority_reg);

        // Очищаем старое значение и устанавливаем новое
        let bit_shift = bit * 8;
        val &= !(0xFF << bit_shift);
        val |= u32::from(priority.raw()) << bit_shift;

        self.distributor.write_reg(priority_reg, val);
    }

    /// Назначает целевые CPU для SPI через GICD_ITARGETSR.
    ///
    /// Поле прерывания - 8-битная маска, бит N = CPU interface N.
    pub(super) fn set_target_cpu(&self, irq: IrqNumber, target: CpuMask) {
        if !matches!(IrqType::from_irq_number(irq), IrqType::Spi) {
            return;
        }

        let (reg, bit) = bit_offset(irq, 8);

        let target_cpu_reg = GICD_ITARGETSR.with_offset(reg * 4);
        let mut val = self.distributor.read_reg(target_cpu_reg);

        // Очищаем старое значение и устанавливаем новое
        let bit_shift = bit * 8;
        val &= !(0xFF << bit_shift);
        val |= u32::from(target.raw()) << bit_shift;

        self.distributor.write_reg(target_cpu_reg, val);
    }

    /// Конфигурирует триггер линии через GICD_ICFGR (2 бита на IRQ; старший бит
    /// поля: `1` = edge, `0` = level). Вызывать до enable: переконфигурация
    /// включённой линии по спеке GIC UNPREDICTABLE.
    pub(super) fn set_config(&self, irq: IrqNumber, trigger: TriggerType) {
        let (reg, entry) = bit_offset(irq, 2);
        let trigger_bit = entry * 2 + 1;
        let reg_addr = GICD_ICFGR.with_offset(reg * 4);

        let mut val = self.distributor.read_reg(reg_addr);
        match trigger {
            TriggerType::Edge => val |= 1 << trigger_bit,
            TriggerType::Level => val &= !(1 << trigger_bit),
        }
        self.distributor.write_reg(reg_addr, val);
    }

    fn acknowledge(&mut self) -> Option<IrqNumber> {
        let raw = (self.cpu_interface.read_reg(GICC_IAR) & 0x3FF) as u16;

        match IrqType::from_irq_number(IrqNumber::new(raw)) {
            IrqType::Spurious => None,
            _ => Some(IrqNumber::new(raw)),
        }
    }

    fn end_of_interrupt(&self, irq: IrqNumber) {
        self.cpu_interface
            .write_reg(GICC_EOIR, u32::from(irq.raw()));
    }
}

impl super::super::DispatchController for Gicv2Controller {
    /// Подтверждает и деактивирует (EOI) один pending IRQ, возвращая его хендлер.
    fn dispatch_interrupt(&mut self) -> Option<Arc<dyn IrqHandler>> {
        let irq = self.acknowledge()?;
        let handler = self.handlers.get(&irq).map(|bound| bound.handler.clone());

        self.end_of_interrupt(irq);

        handler
    }
}
