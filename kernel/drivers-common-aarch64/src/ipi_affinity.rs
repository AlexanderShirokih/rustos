//! Раскладка [`IpiTarget`] для AArch64: уровни affinity из `MPIDR_EL1`.

use drivers_common::services::interrupts::IpiTarget;

/// Affinity целевого CPU по уровням.
///
/// Значение [`IpiTarget`] непрозрачно для арх-нейтрального слоя. Его раскладка
/// объявлена здесь один раз. Её заполняет платформенный слой, а читают
/// контроллеры прерываний.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct IpiAffinity {
    pub aff0: u8,
    pub aff1: u8,
    pub aff2: u8,
    pub aff3: u8,
}

impl IpiAffinity {
    /// Раскладывает значение в формате `MPIDR_EL1` по уровням.
    pub const fn from_mpidr(mpidr: u64) -> Self {
        Self {
            aff0: (mpidr & 0xFF) as u8,
            aff1: ((mpidr >> 8) & 0xFF) as u8,
            aff2: ((mpidr >> 16) & 0xFF) as u8,
            aff3: ((mpidr >> 32) & 0xFF) as u8,
        }
    }

    pub const fn from_target(target: IpiTarget) -> Self {
        Self::from_mpidr(target.raw())
    }

    pub const fn to_target(self) -> IpiTarget {
        IpiTarget::new(
            ((self.aff3 as u64) << 32)
                | ((self.aff2 as u64) << 16)
                | ((self.aff1 as u64) << 8)
                | (self.aff0 as u64),
        )
    }
}

const _: () = assert!(IpiAffinity::from_mpidr(0x0000_0003_0002_0104).aff3 == 3);
const _: () = assert!(IpiAffinity::from_mpidr(0x0000_0003_0002_0104).aff2 == 2);
const _: () = assert!(IpiAffinity::from_mpidr(0x0000_0003_0002_0104).aff1 == 1);
const _: () = assert!(IpiAffinity::from_mpidr(0x0000_0003_0002_0104).aff0 == 4);
const _: () = assert!(
    IpiAffinity::from_mpidr(0x0000_0003_0002_0104)
        .to_target()
        .raw()
        == 0x0000_0003_0002_0104
);
