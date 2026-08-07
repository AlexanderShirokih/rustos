//! Аппаратный контроллер GICv3: Distributor, Redistributor и CPU Interface.

use alloc::{collections::BTreeMap, sync::Arc};
use core::hint::spin_loop;

use drivers_common::services::{
    interrupts::{
        CpuInterfaceError, CpuMask, IpiError, IpiTarget, IrqHandler, IrqNumber, IrqPriority,
        TriggerType,
    },
    mmio::MmioBound,
};
use drivers_common_aarch64::{affinity_frames::AffinityFrames, ipi_affinity::IpiAffinity};
use io::mmio::Reg;
use klog::{debug, warn};

use super::regs::{
    GICD_CTLR, GICD_CTLR_ARE_NS, GICD_CTLR_ENABLE_GRP1NS, GICD_CTLR_RWP, GICD_ICENABLER,
    GICD_ICFGR, GICD_ICPENDR, GICD_IGROUPR, GICD_IGRPMODR, GICD_IPRIORITYR, GICD_IROUTER,
    GICD_ISENABLER, GICD_TYPER, GICR_FRAME_SPAN, GICR_ICENABLER0, GICR_ICFGR0, GICR_ICFGR1,
    GICR_IGROUPR0, GICR_IGRPMODR0, GICR_IPRIORITYR, GICR_ISENABLER0, GICR_TYPER, GICR_WAKER,
    GICR_WAKER_CHILDREN_ASLEEP, GICR_WAKER_PROCESSOR_SLEEP, ITLinesNumber, IrqType,
    PRIORITY_MASK_ALL, bit_offset, frame_owner,
};
use crate::{read_sysreg, write_sysreg};

/// Runtime-объект контроллера прерываний GICv3.
pub(super) struct Gicv3Controller {
    pub(super) distributor: MmioBound,
    /// Отображённая область Redistributor'ов: фреймы всех CPU подряд.
    pub(super) redistributor: MmioBound,
    frames: AffinityFrames,
    pub(super) interrupt_lines: ITLinesNumber,
    pub(super) handlers: BTreeMap<IrqNumber, BoundIrq>,
}

/// Привязанная линия: обработчик и параметры её настройки.
///
/// Регистры SGI и PPI приватны для CPU: настройка, сделанная одним CPU, другому
/// не видна, и на его per-CPU инициализации она повторяется отсюда.
pub(super) struct BoundIrq {
    pub(super) handler: Arc<dyn IrqHandler>,
    pub(super) priority: IrqPriority,
    pub(super) trigger: Option<TriggerType>,
}

const SGI_TARGET_LIST_BITS: u64 = 16;

// Aff3 [55:48], Aff2 [39:32], INTID [27:24], Aff1 [23:16], TargetList [15:0];
// IRM (бит 40) нулевой - рассылка адресная.
const fn sgi1r_value(irq: IrqNumber, affinity: IpiAffinity) -> u64 {
    ((affinity.aff3 as u64) << 48)
        | ((affinity.aff2 as u64) << 32)
        | ((irq.raw() as u64 & 0xF) << 24)
        | ((affinity.aff1 as u64) << 16)
        | (1u64 << affinity.aff0)
}

const _: () = assert!(sgi1r_value(IrqNumber::new(0), IpiAffinity::from_mpidr(0)) == 1);
// Aff3=3, Aff2=2, Aff1=1, Aff0=4, INTID=5.
const _: () = assert!(
    sgi1r_value(
        IrqNumber::new(5),
        IpiAffinity::from_mpidr(0x0000_0003_0002_0104)
    ) == 0x0003_0002_0501_0010
);

// SAFETY: Gicv3Controller содержит только MmioBound (volatile-доступ),
// снятую с железа таблицу фреймов и BTreeMap. Синхронизация обеспечивается
// Mutex на уровне сервиса.
unsafe impl Send for Gicv3Controller {}

impl Gicv3Controller {
    pub(super) fn new(
        distributor: MmioBound,
        redistributor: MmioBound,
        redistributor_stride: usize,
    ) -> Self {
        let interrupt_lines = Self::read_interrupt_lines(&distributor);
        let frames = Self::scan_redistributor_frames(&redistributor, redistributor_stride);

        Self {
            distributor,
            redistributor,
            frames,
            interrupt_lines,
            handlers: BTreeMap::new(),
        }
    }

    pub(super) fn init(&self) {
        Self::set_irq_mask();

        debug!("Enabling GICv3");
        debug!("  GICD @ {}", self.distributor.base());
        debug!("  GICR @ {}", self.redistributor.base());

        Self::enable_sre();
        self.init_distributor();
        if let Err(error) = self.init_this_cpu() {
            warn!("GICv3: the boot CPU interface is not configured: {error}");
        }
        self.enable_distributor();
    }

    /// Настраивает Redistributor и CPU interface текущего CPU. Distributor
    /// общий для всех CPU и здесь не участвует.
    ///
    /// Отказ значит, что прерывания этому CPU не доставляются. Оставляет IRQ
    /// замаскированными на текущем CPU.
    pub(super) fn init_this_cpu(&self) -> Result<(), CpuInterfaceError> {
        Self::set_irq_mask();
        Self::enable_sre();

        let frame = self
            .current_redistributor_frame()
            .ok_or(CpuInterfaceError::NotFound)?;

        self.wakeup_redistributor(frame);
        self.init_redistributor_sgi_ppi(frame);
        Self::configure_cpu_interface();
        self.restore_private_lines();

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

    /// Рассылает SGI процессору по его affinity через `ICC_SGI1R_EL1`.
    ///
    /// `TargetList` адресует Aff0 отдельным битом: в одной группе адресуются
    /// шестнадцать процессоров. За этим пределом нужен `RS` из GICv3.1.
    pub(super) fn send_sgi(irq: IrqNumber, target: IpiTarget) -> Result<(), IpiError> {
        if !matches!(IrqType::from_irq_number(irq), IrqType::Sgi) {
            return Err(IpiError::NotAnIpi);
        }

        let affinity = IpiAffinity::from_target(target);
        if u64::from(affinity.aff0) >= SGI_TARGET_LIST_BITS {
            return Err(IpiError::UnreachableTarget);
        }

        let value = sgi1r_value(irq, affinity);

        // SAFETY: ICC_SGI1R_EL1 доступен на EL1 при `ICC_SRE_EL1.SRE=1`, что
        // ставит `enable_sre`. `dsb ishst` публикует записи, ради которых
        // прерывание и посылается, до его генерации; `isb` не даёт отправке
        // отстать от возврата из функции.
        unsafe {
            core::arch::asm!(
                "dsb    ishst",
                "msr    icc_sgi1r_el1, {value}",
                "isb",
                value = in(reg) value,
                options(nostack, preserves_flags),
            );
        }

        Ok(())
    }

    fn current_redistributor_frame(&self) -> Option<usize> {
        // SAFETY: MPIDR_EL1 доступен на EL1 и только читается.
        let mpidr = unsafe { read_sysreg!(mpidr_el1) };

        self.frames.offset_of(IpiAffinity::from_mpidr(mpidr))
    }

    fn scan_redistributor_frames(redistributor: &MmioBound, stride: usize) -> AffinityFrames {
        AffinityFrames::scan(stride, |offset| {
            redistributor
                .contains(offset, GICR_FRAME_SPAN)
                .then(|| frame_owner(redistributor.read_reg(GICR_TYPER.with_offset(offset))))
        })
    }

    pub(super) fn enable_global() {
        Self::clear_irq_mask();
    }

    pub(super) fn disable_global() {
        Self::set_irq_mask();
    }

    pub(super) fn enable(&self, irq: IrqNumber) {
        self.write_enable_bit(irq, GICR_ISENABLER0, GICD_ISENABLER);
    }

    pub(super) fn disable(&self, irq: IrqNumber) {
        self.write_enable_bit(irq, GICR_ICENABLER0, GICD_ICENABLER);
    }

    fn write_enable_bit(
        &self,
        irq: IrqNumber,
        redistributor_reg: Reg<u32>,
        distributor_reg: Reg<u32>,
    ) {
        match IrqType::from_irq_number(irq) {
            IrqType::Sgi | IrqType::Ppi => {
                let Some(frame) = self.current_redistributor_frame() else {
                    warn!(
                        "GICv3: no redistributor frame for this CPU, IRQ {} left as is",
                        irq.raw()
                    );
                    return;
                };
                self.redistributor
                    .write_reg(redistributor_reg.with_offset(frame), 1u32 << irq.raw());
            }
            IrqType::Spi => {
                let (reg, bit) = bit_offset(irq, 1);
                self.distributor
                    .write_reg(distributor_reg.with_offset(reg * 4), 1 << bit);
            }
            IrqType::Spurious => {}
        }
    }

    pub(super) fn set_priority(&self, irq: IrqNumber, priority: IrqPriority) {
        match IrqType::from_irq_number(irq) {
            IrqType::Sgi | IrqType::Ppi => {
                let Some(frame) = self.current_redistributor_frame() else {
                    warn!(
                        "GICv3: no redistributor frame for this CPU, IRQ {} keeps priority",
                        irq.raw()
                    );
                    return;
                };
                let (reg, bit) = bit_offset(irq, 8);
                let reg_desc = GICR_IPRIORITYR.with_offset(frame + reg * 4);
                let mut val: u32 = self.redistributor.read_reg(reg_desc);
                let shift = bit * 8;
                val &= !(0xFF << shift);
                val |= u32::from(priority.raw()) << shift;
                self.redistributor.write_reg(reg_desc, val);
            }
            IrqType::Spi => {
                let (reg, bit) = bit_offset(irq, 8);
                let reg_desc = GICD_IPRIORITYR.with_offset(reg * 4);
                let mut val: u32 = self.distributor.read_reg(reg_desc);
                let shift = bit * 8;
                val &= !(0xFF << shift);
                val |= u32::from(priority.raw()) << shift;
                self.distributor.write_reg(reg_desc, val);
            }
            IrqType::Spurious => {}
        }
    }

    /// Маршрутизирует SPI через GICD_IROUTER.
    ///
    /// Один CPU в маске: Aff0 = индекс PE в affinity-кластере 0 (Aff1..Aff3 = 0).
    /// Несколько CPU: IRM=1 (бит 31), любой участвующий PE.
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
            u64::from(target.raw().trailing_zeros())
        };

        self.distributor
            .write_reg(GICD_IROUTER.with_offset(router_offset), route);
    }

    fn enable_sre() {
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

        // ARE_NS до конфигурации групп: записи групп эффективны только при ARE_NS=1.
        self.distributor.write_reg(GICD_CTLR, GICD_CTLR_ARE_NS);
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
        for i in 0..spi_count.div_ceil(4) {
            self.distributor
                .write_reg(GICD_IPRIORITYR.with_offset((8 + i) * 4), 0xFFFF_FFFFu32);
        }

        // Level-sensitive конфигурация для всех SPI (2 бита на IRQ, банки 2+)
        for i in 0..spi_count.div_ceil(16) {
            self.distributor
                .write_reg(GICD_ICFGR.with_offset((2 + i) * 4), 0u32);
        }

        // Маршрутизация всех SPI на CPU0 по умолчанию (Aff0=0, IRM=0)
        for i in 0..spi_count {
            self.distributor
                .write_reg(GICD_IROUTER.with_offset(i * 8), 0u64);
        }
    }

    fn wakeup_redistributor(&self, frame: usize) {
        let waker_reg = GICR_WAKER.with_offset(frame);
        let waker: u32 = self.redistributor.read_reg(waker_reg);

        if (waker & GICR_WAKER_PROCESSOR_SLEEP) == 0 {
            return;
        }

        self.redistributor
            .write_reg(waker_reg, waker & !GICR_WAKER_PROCESSOR_SLEEP);

        // Ожидание снятия флага ChildrenAsleep
        while (self.redistributor.read_reg(waker_reg) & GICR_WAKER_CHILDREN_ASLEEP) != 0 {
            spin_loop();
        }
    }

    fn init_redistributor_sgi_ppi(&self, frame: usize) {
        // Отключить все SGI/PPI перед настройкой
        self.redistributor
            .write_reg(GICR_ICENABLER0.with_offset(frame), u32::MAX);

        // Group 1 NS: IGROUPR0=0xFF...FF, IGRPMODR0=0
        self.redistributor
            .write_reg(GICR_IGROUPR0.with_offset(frame), u32::MAX);
        self.redistributor
            .write_reg(GICR_IGRPMODR0.with_offset(frame), 0u32);

        // Наинизший приоритет для всех SGI/PPI (8 регистров, 4 IRQ каждый)
        for i in 0..8usize {
            self.redistributor
                .write_reg(GICR_IPRIORITYR.with_offset(frame + i * 4), 0xFFFF_FFFFu32);
        }

        // Level-sensitive конфигурация
        self.redistributor
            .write_reg(GICR_ICFGR0.with_offset(frame), 0u32);
        self.redistributor
            .write_reg(GICR_ICFGR1.with_offset(frame), 0u32);
    }

    // Distributor общий для всех CPU: включается однократно.
    fn enable_distributor(&self) {
        self.distributor
            .write_reg(GICD_CTLR, GICD_CTLR_ENABLE_GRP1NS | GICD_CTLR_ARE_NS);
        self.wait_gicd_rwp();
    }

    fn configure_cpu_interface() {
        // SAFETY: Запись в ICC_PMR_EL1 устанавливает порог приоритета.
        // Значение 0xFF пропускает все прерывания.
        unsafe { write_sysreg!(icc_pmr_el1, PRIORITY_MASK_ALL) };

        // SAFETY: Запись 0 в ICC_BPR1_EL1 настраивает binary point для Group 1:
        // все 8 бит приоритета участвуют в сравнении с PMR.
        unsafe { write_sysreg!(icc_bpr1_el1, 0u64) };

        // SAFETY: Запись 1 в ICC_IGRPEN1_EL1 разрешает доставку прерываний группы 1
        // на текущий CPU. Должна выполняться после инициализации GICR.
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

    fn acknowledge() -> Option<IrqNumber> {
        // SAFETY: Чтение ICC_IAR1_EL1 возвращает INTID текущего pending прерывания группы 1
        // и переводит его в active-состояние. Побочный эффект допустим - это штатная операция.
        let raw = (unsafe { read_sysreg!(icc_iar1_el1) } & 0x3FF) as u16;

        match IrqType::from_irq_number(IrqNumber::new(raw)) {
            IrqType::Spurious => None,
            _ => Some(IrqNumber::new(raw)),
        }
    }

    fn end_of_interrupt(irq: IrqNumber) {
        // SAFETY: Запись INTID в ICC_EOIR1_EL1 переводит прерывание из active в inactive,
        // сигнализируя GIC об окончании обработки прерывания группы 1.
        unsafe { write_sysreg!(icc_eoir1_el1, u64::from(irq.raw())) };
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

    pub(super) fn set_config(&self, irq: IrqNumber, trigger: TriggerType) {
        // PPI приватна для CPU и живёт в GICR_ICFGR1 своего redistributor, SPI -
        // в GICD_ICFGR распределителя; триггер SGI задан архитектурой.
        match IrqType::from_irq_number(irq) {
            IrqType::Ppi => self.set_private_config(irq, trigger),
            IrqType::Spi => self.set_shared_config(irq, trigger),
            IrqType::Sgi | IrqType::Spurious => {}
        }
    }

    fn set_private_config(&self, irq: IrqNumber, trigger: TriggerType) {
        let trigger_bit = (irq.raw() as usize % 16) * 2 + 1;

        let Some(frame) = self.current_redistributor_frame() else {
            warn!(
                "GICv3: no redistributor frame for this CPU, IRQ {} keeps its trigger",
                irq.raw()
            );
            return;
        };

        Self::write_trigger_bit(
            &self.redistributor,
            GICR_ICFGR1.with_offset(frame),
            trigger_bit,
            trigger,
        );
    }

    fn set_shared_config(&self, irq: IrqNumber, trigger: TriggerType) {
        let (reg, entry) = bit_offset(irq, 2);
        Self::write_trigger_bit(
            &self.distributor,
            GICD_ICFGR.with_offset(reg * 4),
            entry * 2 + 1,
            trigger,
        );
    }

    fn write_trigger_bit(mmio: &MmioBound, reg: Reg<u32>, bit: usize, trigger: TriggerType) {
        let mut val = mmio.read_reg(reg);
        match trigger {
            TriggerType::Edge => val |= 1 << bit,
            TriggerType::Level => val &= !(1 << bit),
        }
        mmio.write_reg(reg, val);
    }
}

impl super::super::DispatchController for Gicv3Controller {
    /// Подтверждает и деактивирует (EOI) один pending IRQ, возвращая его хендлер.
    fn dispatch_interrupt(&mut self) -> Option<Arc<dyn IrqHandler>> {
        let irq = Self::acknowledge()?;
        let handler = self.handlers.get(&irq).map(|bound| bound.handler.clone());

        Self::end_of_interrupt(irq);

        handler
    }
}
