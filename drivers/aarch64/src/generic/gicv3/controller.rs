//! Аппаратный контроллер GICv3: Distributor, Redistributor и CPU Interface.

use super::regs::*;
use crate::{read_sysreg, write_sysreg};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use core::hint::spin_loop;
use drivers_common::services::interrupts::{CpuMask, IrqHandler, IrqNumber, IrqPriority};
use drivers_common::services::mmio::MmioBound;
use klog::debug;

/// Runtime-объект контроллера прерываний GICv3, публикуемый через capability.
pub(super) struct Gicv3Controller {
    pub(super) distributor: MmioBound,
    /// GICR-регион текущего CPU (RD_base + SGI_base в одном маппинге).
    pub(super) redistributor: MmioBound,
    pub(super) interrupt_lines: ITLinesNumber,
    pub(super) handlers: BTreeMap<IrqNumber, Box<dyn IrqHandler>>,
}

// SAFETY: Gicv3Controller содержит только MmioBound (volatile-доступ) и BTreeMap.
// Синхронизация обеспечивается Mutex на уровне сервиса.
unsafe impl Send for Gicv3Controller {}

impl Gicv3Controller {
    pub(super) fn new(distributor: MmioBound, redistributor: MmioBound) -> Self {
        let interrupt_lines = Self::read_interrupt_lines(&distributor);

        Self {
            distributor,
            redistributor,
            interrupt_lines,
            handlers: BTreeMap::new(),
        }
    }

    pub(super) fn init(&self) {
        Self::set_irq_mask();

        debug!("Enabling GICv3");
        debug!("  GICD @ {}", self.distributor.base());
        debug!("  GICR @ {}", self.redistributor.base());

        self.enable_sre();
        self.init_distributor();
        self.wakeup_redistributor();
        self.init_redistributor_sgi_ppi();
        self.configure_cpu_interface();
    }

    pub(super) fn enable_global(&self) {
        Self::clear_irq_mask();
    }

    pub(super) fn disable_global(&self) {
        Self::set_irq_mask();
    }

    pub(super) fn enable(&self, irq: IrqNumber) {
        match IrqType::from_irq_number(irq) {
            IrqType::Sgi | IrqType::Ppi => {
                self.redistributor
                    .write_reg(GICR_ISENABLER0, 1u32 << irq.raw());
            }
            IrqType::Spi => {
                let (reg, bit) = bit_offset(irq, 1);
                self.distributor
                    .write_reg(GICD_ISENABLER.with_offset(reg * 4), 1 << bit);
            }
            IrqType::Spurious => {}
        }
    }

    pub(super) fn disable(&self, irq: IrqNumber) {
        match IrqType::from_irq_number(irq) {
            IrqType::Sgi | IrqType::Ppi => {
                self.redistributor
                    .write_reg(GICR_ICENABLER0, 1u32 << irq.raw());
            }
            IrqType::Spi => {
                let (reg, bit) = bit_offset(irq, 1);
                self.distributor
                    .write_reg(GICD_ICENABLER.with_offset(reg * 4), 1 << bit);
            }
            IrqType::Spurious => {}
        }
    }

    pub(super) fn set_priority(&self, irq: IrqNumber, priority: IrqPriority) {
        match IrqType::from_irq_number(irq) {
            IrqType::Sgi | IrqType::Ppi => {
                let (reg, bit) = bit_offset(irq, 8);
                let reg_desc = GICR_IPRIORITYR.with_offset(reg * 4);
                let mut val: u32 = self.redistributor.read_reg(reg_desc);
                let shift = bit * 8;
                val &= !(0xFF << shift);
                val |= (priority.raw() as u32) << shift;
                self.redistributor.write_reg(reg_desc, val);
            }
            IrqType::Spi => {
                let (reg, bit) = bit_offset(irq, 8);
                let reg_desc = GICD_IPRIORITYR.with_offset(reg * 4);
                let mut val: u32 = self.distributor.read_reg(reg_desc);
                let shift = bit * 8;
                val &= !(0xFF << shift);
                val |= (priority.raw() as u32) << shift;
                self.distributor.write_reg(reg_desc, val);
            }
            IrqType::Spurious => {}
        }
    }

    /// Устанавливает affinity routing для SPI.
    ///
    /// Если маска содержит несколько CPU — используется IRM=1 (любой доступный).
    /// Если один CPU — используется специфический Aff0.
    pub(super) fn set_affinity(&self, irq: IrqNumber, target: CpuMask) {
        if !matches!(IrqType::from_irq_number(irq), IrqType::Spi) {
            return;
        }

        let router_offset = (irq.raw() as usize - 32) * 8;

        let route: u64 = if target.raw().count_ones() > 1 {
            // IRM=1: доставить любому доступному PE
            1u64 << 31
        } else {
            // Специфическая маршрутизация: Aff0 = индекс CPU
            target.raw().trailing_zeros() as u64
        };

        self.distributor
            .write_reg(GICD_IROUTER.with_offset(router_offset), route);
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

    // --- Приватные методы инициализации ---

    fn enable_sre(&self) {
        // SAFETY: Запись в ICC_SRE_EL1 включает доступ к системным регистрам GIC CPU Interface.
        // Бит SRE=1 необходим для работы ICC_IAR1_EL1, ICC_EOIR1_EL1 и других ICC_* регистров.
        // ISB гарантирует видимость изменения до следующих инструкций.
        unsafe {
            let sre = read_sysreg!(icc_sre_el1);
            write_sysreg!(icc_sre_el1, sre | 0x1);
            core::arch::asm!("isb", options(nomem, nostack, preserves_flags));
        }
    }

    fn init_distributor(&self) {
        // Отключить Distributor перед настройкой
        self.distributor.write_reg(GICD_CTLR, 0u32);
        self.wait_gicd_rwp();

        // Сбросить все SPI (банки 1+, банк 0 относится к SGI/PPI и управляется через GICR)
        let spi_banks = self.interrupt_lines.saturating_sub(1);

        for bank in 1..=spi_banks {
            let offset = bank * 4;

            self.distributor
                .write_reg(GICD_ICENABLER.with_offset(offset), u32::MAX);
            self.distributor
                .write_reg(GICD_ICPENDR.with_offset(offset), u32::MAX);

            // Group 1 NS: IGROUPR=1, IGRPMODR=0
            self.distributor
                .write_reg(GICD_IGROUPR.with_offset(offset), u32::MAX);
            self.distributor
                .write_reg(GICD_IGRPMODR.with_offset(offset), 0);
        }

        // Установить наинизший приоритет для всех SPI (4 IRQ на регистр, начиная с IRQ 32)
        let spi_count = (self.interrupt_lines * 32).saturating_sub(32);
        for i in 0..(spi_count + 3) / 4 {
            self.distributor
                .write_reg(GICD_IPRIORITYR.with_offset((8 + i) * 4), 0xFFFF_FFFFu32);
        }

        // Level-sensitive конфигурация для всех SPI (2 бита на IRQ, банки 2+)
        for i in 0..(spi_count + 15) / 16 {
            self.distributor
                .write_reg(GICD_ICFGR.with_offset((2 + i) * 4), 0u32);
        }

        // Маршрутизация всех SPI на CPU0 по умолчанию (Aff0=0, IRM=0)
        for i in 0..spi_count {
            self.distributor
                .write_reg(GICD_IROUTER.with_offset(i * 8), 0u64);
        }
    }

    fn wakeup_redistributor(&self) {
        let waker: u32 = self.redistributor.read_reg(GICR_WAKER);

        if (waker & GICR_WAKER_PROCESSOR_SLEEP) == 0 {
            return;
        }

        self.redistributor
            .write_reg(GICR_WAKER, waker & !GICR_WAKER_PROCESSOR_SLEEP);

        // Ожидание снятия флага ChildrenAsleep
        while (self.redistributor.read_reg(GICR_WAKER) & GICR_WAKER_CHILDREN_ASLEEP) != 0 {
            spin_loop();
        }
    }

    fn init_redistributor_sgi_ppi(&self) {
        // Отключить все SGI/PPI перед настройкой
        self.redistributor.write_reg(GICR_ICENABLER0, u32::MAX);

        // Group 1 NS: IGROUPR0=0xFF...FF, IGRPMODR0=0
        self.redistributor.write_reg(GICR_IGROUPR0, u32::MAX);
        self.redistributor.write_reg(GICR_IGRPMODR0, 0u32);

        // Наинизший приоритет для всех SGI/PPI (8 регистров, 4 IRQ каждый)
        for i in 0..8usize {
            self.redistributor
                .write_reg(GICR_IPRIORITYR.with_offset(i * 4), 0xFFFF_FFFFu32);
        }

        // Level-sensitive конфигурация
        self.redistributor.write_reg(GICR_ICFGR0, 0u32);
        self.redistributor.write_reg(GICR_ICFGR1, 0u32);
    }

    fn configure_cpu_interface(&self) {
        // SAFETY: Запись в ICC_PMR_EL1 устанавливает порог приоритета.
        // Значение 0xFF пропускает все прерывания.
        unsafe { write_sysreg!(icc_pmr_el1, PRIORITY_MASK_ALL) };

        // SAFETY: Запись 0 в ICC_BPR1_EL1 настраивает binary point для Group 1:
        // все 8 бит приоритета участвуют в сравнении с PMR.
        unsafe { write_sysreg!(icc_bpr1_el1, 0u64) };

        // Включить Distributor с ARE_NS и Group 1 NS
        self.distributor
            .write_reg(GICD_CTLR, GICD_CTLR_ENABLE_GRP1NS | GICD_CTLR_ARE_NS);
        self.wait_gicd_rwp();

        // SAFETY: Запись 1 в ICC_IGRPEN1_EL1 разрешает доставку прерываний группы 1
        // на текущий CPU. Должна выполняться после инициализации GICD и GICR.
        unsafe { write_sysreg!(icc_igrpen1_el1, 1u64) };
    }

    fn wait_gicd_rwp(&self) {
        while (self.distributor.read_reg(GICD_CTLR) & GICD_CTLR_RWP) != 0 {
            spin_loop();
        }
    }

    fn read_interrupt_lines(distributor: &MmioBound) -> ITLinesNumber {
        let typer: u32 = distributor.read_reg(GICD_TYPER);
        let it_lines = typer & 0x1F;
        (it_lines + 1) as usize
    }

    fn acknowledge(&self) -> Option<IrqNumber> {
        // SAFETY: Чтение ICC_IAR1_EL1 возвращает INTID текущего pending прерывания группы 1
        // и переводит его в active-состояние. Побочный эффект допустим — это штатная операция.
        let raw = (unsafe { read_sysreg!(icc_iar1_el1) } & 0x3FF) as u16;

        match IrqType::from_irq_number(IrqNumber::new(raw)) {
            IrqType::Spurious => None,
            _ => Some(IrqNumber::new(raw)),
        }
    }

    fn end_of_interrupt(&self, irq: IrqNumber) {
        // SAFETY: Запись INTID в ICC_EOIR1_EL1 переводит прерывание из active в inactive,
        // сигнализируя GIC об окончании обработки прерывания группы 1.
        unsafe { write_sysreg!(icc_eoir1_el1, irq.raw() as u64) };
    }

    fn set_irq_mask() {
        // SAFETY: Установка бита I в DAIF запрещает обработку IRQ на текущем CPU.
        unsafe {
            core::arch::asm!("msr daifset, #0b0010", options(nostack, preserves_flags));
        }
    }

    fn clear_irq_mask() {
        // SAFETY: Очистка бита I в DAIF разрешает обработку IRQ на текущем CPU.
        unsafe {
            core::arch::asm!("msr daifclr, #0b0010", options(nostack, preserves_flags));
        }
    }
}
