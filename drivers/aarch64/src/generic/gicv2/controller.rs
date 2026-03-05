//! Аппаратный контроллер GICv2: Distributor и CPU Interface.

use alloc::{boxed::Box, collections::BTreeMap};

use drivers_common::services::{
    interrupts::{CpuMask, IrqHandler, IrqNumber, IrqPriority},
    mmio::MmioBound,
};
use klog::debug;

use super::regs::*;

/// Runtime-объект контроллера прерываний GICv2, публикуемый через capability.
pub(super) struct Gicv2Controller {
    pub(super) distributor: MmioBound,
    pub(super) cpu_interface: MmioBound,
    pub(super) interrupt_lines: ITLinesNumber,
    pub(super) handlers: BTreeMap<IrqNumber, Box<dyn IrqHandler>>,
}

// SAFETY: Gicv2Controller содержит только Mmio (который Sync).
// Все операции с Mmio выполняются через volatile, что безопасно для многопоточности.
unsafe impl Send for Gicv2Controller {}

impl Gicv2Controller {
    pub(super) fn new(distributor: MmioBound, cpu_interface: MmioBound) -> Self {
        Self {
            handlers: BTreeMap::new(),
            interrupt_lines: Self::get_interrupts_lines(&distributor),
            distributor,
            cpu_interface,
        }
    }

    pub(super) fn init(&self) {
        Self::set_irq_mask();

        debug!("Enabling GICv2");
        debug!("  GICD @ {}", self.distributor.base());
        debug!("  GICC @ {}", self.cpu_interface.base());

        self.init_distributor();
        self.init_cpu_interface();
    }

    pub(super) fn enable_global(&self) {
        Self::clear_irq_mask()
    }

    pub(super) fn disable_global(&self) {
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
        val |= (priority.raw() as u32) << bit_shift;

        self.distributor.write_reg(priority_reg, val);
    }

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
        val |= (target.raw() as u32) << bit_shift;

        self.distributor.write_reg(target_cpu_reg, val);
    }

    pub(super) fn dispatch_interrupt(&mut self) {
        let Some(irq) = self.acknowledge() else {
            return;
        };

        if let Some(handler) = self.handlers.get(&irq) {
            handler.handle();
        }

        self.end_of_interrupt(irq);
    }

    fn acknowledge(&mut self) -> Option<IrqNumber> {
        let raw = (self.cpu_interface.read_reg(GICC_IAR) & 0x3FF) as u16;

        match IrqType::from_irq_number(IrqNumber::new(raw)) {
            IrqType::Spurious => None,
            _ => Some(IrqNumber::new(raw)),
        }
    }

    fn end_of_interrupt(&self, irq: IrqNumber) {
        self.cpu_interface.write_reg(GICC_EOIR, irq.raw() as u32);
    }
}
