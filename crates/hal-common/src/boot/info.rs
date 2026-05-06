//! Описание загрузочной информации, передаваемой от загрузчика к ядру.

use memory::physical_address::PhysicalAddress;

/// Информация, переданная загрузчиком при старте.
#[non_exhaustive]
pub struct BootInfo {
    /// Описание аппаратного обеспечения.
    pub hw_description: HwDescription,
}

impl BootInfo {
    /// Создать `BootInfo` с FDT-описанием по физическому адресу `phys`.
    pub const fn new_fdt(phys: PhysicalAddress) -> Self {
        Self {
            hw_description: HwDescription::Fdt(FdtBlob { phys }),
        }
    }
}

/// Источник описания аппаратного обеспечения.
#[non_exhaustive]
pub enum HwDescription {
    /// Device Tree Blob по физическому адресу.
    Fdt(FdtBlob),
    /// ACPI-таблицы (не реализовано).
    Acpi(AcpiTables),
}

/// Физический адрес FDT-блоба.
pub struct FdtBlob {
    pub phys: PhysicalAddress,
}

/// ACPI-таблицы (зарезервировано для будущего использования).
pub struct AcpiTables {
    _private: (),
}

#[cfg(test)]
mod tests {
    use memory::physical_address::PhysicalAddress;

    use super::{BootInfo, HwDescription};

    #[test]
    fn new_fdt_builds_expected_variant() {
        let phys = PhysicalAddress::new(0x4000_0000);
        let info = BootInfo::new_fdt(phys);
        let HwDescription::Fdt(blob) = info.hw_description else {
            panic!("ожидался вариант Fdt");
        };
        assert_eq!(blob.phys.as_usize(), 0x4000_0000);
    }

    #[test]
    fn boot_info_size_stable() {
        // HwDescription = дискриминант + PhysicalAddress (usize) = 2 * usize
        assert_eq!(
            core::mem::size_of::<BootInfo>(),
            core::mem::size_of::<HwDescription>()
        );
    }
}
