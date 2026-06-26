//! Разбор GIC interrupt-specifier из FDT-свойства `interrupts`.

use core::mem::size_of;

use drivers_common::services::interrupts::{IrqNumber, TriggerType};

/// Число ячеек в одном GIC interrupt-specifier: `type`, `number`, `flags`.
const SPECIFIER_CELLS: usize = 3;
const CELL_SIZE: usize = size_of::<u32>();
const SPECIFIER_SIZE: usize = SPECIFIER_CELLS * CELL_SIZE;

const GIC_TYPE_SPI: u32 = 0;
const GIC_TYPE_PPI: u32 = 1;

/// Распарсенный GIC interrupt-specifier: INTID линии и тип триггера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GicInterrupt {
    pub irq: IrqNumber,
    pub trigger: TriggerType,
}

/// Разбирает `index`-й specifier (3 ячейки big-endian) свойства `interrupts`:
/// `type` (SPI/PPI) и `number` дают INTID (`+32`/`+16`), `flags` - [`TriggerType`].
/// `None` при некорректной длине, отсутствии specifier'а или неизвестном типе.
pub fn parse_gic_interrupt(raw: &[u8], index: usize) -> Option<GicInterrupt> {
    if raw.is_empty() || !raw.len().is_multiple_of(SPECIFIER_SIZE) {
        return None;
    }
    let base = index.checked_mul(SPECIFIER_SIZE)?;
    let kind = read_be_u32(raw, base)?;
    let number = read_be_u32(raw, base + CELL_SIZE)?;
    let flags = read_be_u32(raw, base + 2 * CELL_SIZE)?;

    let intid = match kind {
        GIC_TYPE_SPI => number.checked_add(32)?,
        GIC_TYPE_PPI => number.checked_add(16)?,
        _ => return None,
    };
    if intid > u32::from(u16::MAX) {
        return None;
    }

    Some(GicInterrupt {
        irq: IrqNumber::new(intid as u16),
        trigger: trigger_from_flags(flags),
    })
}

/// Флаги триггера (DT GIC binding): биты 0..4 - `1`=edge-rising, `2`=edge-falling,
/// `4`=level-high, `8`=level-low. Неизвестное сводится к `Level` (безопасный дефолт).
fn trigger_from_flags(flags: u32) -> TriggerType {
    match flags & 0xF {
        1 | 2 => TriggerType::Edge,
        _ => TriggerType::Level,
    }
}

fn read_be_u32(raw: &[u8], offset: usize) -> Option<u32> {
    let end = offset.checked_add(CELL_SIZE)?;
    let bytes = raw.get(offset..end)?;
    Some(u32::from_be_bytes(bytes.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(kind: u32, number: u32, flags: u32) -> [u8; SPECIFIER_SIZE] {
        let mut out = [0u8; SPECIFIER_SIZE];
        out[0..4].copy_from_slice(&kind.to_be_bytes());
        out[4..8].copy_from_slice(&number.to_be_bytes());
        out[8..12].copy_from_slice(&flags.to_be_bytes());
        out
    }

    #[test]
    fn spi_level_high() {
        let raw = spec(GIC_TYPE_SPI, 2, 4);
        let got = parse_gic_interrupt(&raw, 0).unwrap();
        assert_eq!(got.irq.raw(), 34);
        assert_eq!(got.trigger, TriggerType::Level);
    }

    #[test]
    fn ppi_edge_rising() {
        let raw = spec(GIC_TYPE_PPI, 11, 1);
        let got = parse_gic_interrupt(&raw, 0).unwrap();
        assert_eq!(got.irq.raw(), 27);
        assert_eq!(got.trigger, TriggerType::Edge);
    }

    #[test]
    fn picks_specifier_by_index() {
        let mut raw = [0u8; SPECIFIER_SIZE * 2];
        raw[..SPECIFIER_SIZE].copy_from_slice(&spec(GIC_TYPE_SPI, 1, 4));
        raw[SPECIFIER_SIZE..].copy_from_slice(&spec(GIC_TYPE_PPI, 11, 1));
        let got = parse_gic_interrupt(&raw, 1).unwrap();
        assert_eq!(got.irq.raw(), 27);
        assert_eq!(got.trigger, TriggerType::Edge);
    }

    #[test]
    fn rejects_unknown_type_and_short_input() {
        assert!(parse_gic_interrupt(&spec(7, 0, 4), 0).is_none());
        assert!(parse_gic_interrupt(&[0u8; 8], 0).is_none());
        assert!(parse_gic_interrupt(&spec(GIC_TYPE_SPI, 0, 4), 1).is_none());
    }
}
