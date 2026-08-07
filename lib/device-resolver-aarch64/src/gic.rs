use device_resolver::{
    ControllerBinding, ControllerBindings, ControllerId, DeviceInterrupt, InterruptLine,
    InterruptSpecifier,
};
use syscall::IrqTrigger;

const GIC_COMPATIBLE: &[&str] = &["arm,gic-v3", "arm,gic-400", "arm,cortex-a15-gic"];

const GIC_SPECIFIER_CELLS: usize = 3;

const GIC_KIND_SPI: u32 = 0;
const GIC_KIND_PPI: u32 = 1;

const GIC_SPI_BASE: u32 = 32;
const GIC_PPI_BASE: u32 = 16;

const GIC_TRIGGER_MASK: u32 = 0xF;
const GIC_EDGE_RISING: u32 = 1;
const GIC_EDGE_FALLING: u32 = 2;

/// Реестр биндингов контроллеров прерываний платформы aarch64.
pub const PLATFORM_BINDINGS: ControllerBindings =
    ControllerBindings::new(&[ControllerBinding::new(GIC_COMPATIBLE, decode_gic_specifier)]);

fn decode_gic_specifier(specifier: InterruptSpecifier<'_>) -> Option<DeviceInterrupt> {
    if specifier.cell_count() != GIC_SPECIFIER_CELLS {
        return None;
    }
    let kind = specifier.cell(0)?;
    let number = specifier.cell(1)?;
    let flags = specifier.cell(2)?;

    // Пространство INTID контроллера плоское: SPI и PPI ложатся в него со
    // своими смещениями.
    let intid = match kind {
        GIC_KIND_SPI => number.checked_add(GIC_SPI_BASE)?,
        GIC_KIND_PPI => number.checked_add(GIC_PPI_BASE)?,
        _ => return None,
    };
    let trigger = match flags & GIC_TRIGGER_MASK {
        GIC_EDGE_RISING | GIC_EDGE_FALLING => IrqTrigger::Edge,
        _ => IrqTrigger::Level,
    };

    Some(DeviceInterrupt {
        line: InterruptLine::new(ControllerId::PLATFORM, u16::try_from(intid).ok()?),
        trigger,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL_SIZE: usize = size_of::<u32>();

    fn gic_specifier(kind: u32, number: u32, flags: u32) -> [u8; GIC_SPECIFIER_CELLS * CELL_SIZE] {
        let mut cells = [0u8; GIC_SPECIFIER_CELLS * CELL_SIZE];
        cells[0..4].copy_from_slice(&kind.to_be_bytes());
        cells[4..8].copy_from_slice(&number.to_be_bytes());
        cells[8..12].copy_from_slice(&flags.to_be_bytes());
        cells
    }

    fn decode(kind: u32, number: u32, flags: u32) -> Option<DeviceInterrupt> {
        let cells = gic_specifier(kind, number, flags);
        PLATFORM_BINDINGS
            .lookup("arm,gic-v3")
            .expect("gic binding")
            .decode(InterruptSpecifier::new(&cells))
    }

    #[test]
    fn gic_spi_specifier_carries_the_line_offset_and_a_level_trigger() {
        let interrupt = decode(GIC_KIND_SPI, 2, 4).expect("spi specifier decodes");

        assert_eq!(interrupt.line.number(), 34);
        assert_eq!(interrupt.line.controller(), ControllerId::PLATFORM);
        assert_eq!(interrupt.trigger, IrqTrigger::Level);
    }

    #[test]
    fn gic_ppi_specifier_carries_the_line_offset_and_an_edge_trigger() {
        let interrupt = decode(GIC_KIND_PPI, 11, 1).expect("ppi specifier decodes");

        assert_eq!(interrupt.line.number(), 27);
        assert_eq!(interrupt.trigger, IrqTrigger::Edge);
    }

    #[test]
    fn gic_falling_edge_is_an_edge_trigger() {
        let interrupt = decode(GIC_KIND_SPI, 0, 2).expect("specifier decodes");

        assert_eq!(interrupt.trigger, IrqTrigger::Edge);
    }

    #[test]
    fn gic_rejects_an_unknown_interrupt_kind_and_a_short_specifier() {
        assert_eq!(decode(7, 0, 4), None);

        let short = [0u8; CELL_SIZE * 2];
        assert_eq!(
            PLATFORM_BINDINGS
                .lookup("arm,gic-v3")
                .expect("gic binding")
                .decode(InterruptSpecifier::new(&short)),
            None
        );
    }

    #[test]
    fn both_gic_generations_lead_to_one_entry() {
        for compatible in ["arm,gic-v3", "arm,cortex-a15-gic"] {
            let binding = PLATFORM_BINDINGS.lookup(compatible).expect("gic binding");
            let cells = gic_specifier(GIC_KIND_SPI, 2, 4);
            assert_eq!(
                binding
                    .decode(InterruptSpecifier::new(&cells))
                    .map(|interrupt| interrupt.line.number()),
                Some(34)
            );
        }
    }

    #[test]
    fn a_controller_outside_the_platform_has_no_binding() {
        assert!(PLATFORM_BINDINGS.lookup("riscv,plic0").is_none());
    }
}
