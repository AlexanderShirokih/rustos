//! Описание загрузочной информации, передаваемой от загрузчика к ядру.

use fdt::devicetree::DeviceTree;
use memory::physical_address::PhysicalAddress;

use crate::scanner::find_initrd_payload;

/// Информация, переданная загрузчиком при старте.
#[non_exhaustive]
pub struct BootInfo {
    /// Описание аппаратного обеспечения.
    pub hw_description: HwDescription,
    /// Optional userland blob, переданный загрузчиком в физической памяти.
    pub userland_blob: Option<BootPayloadRange>,
}

impl BootInfo {
    /// Собирает `BootInfo` из FDT, переданного загрузчиком.
    pub fn from_fdt(phys: PhysicalAddress) -> Self {
        let userland_blob = DeviceTree::from_ptr(phys.as_usize())
            .ok()
            .and_then(|tree| find_initrd_payload(&tree));

        Self::new_fdt_with_payload(phys, userland_blob)
    }

    /// Создать `BootInfo` с FDT-описанием по физическому адресу `phys`.
    pub const fn new_fdt(phys: PhysicalAddress) -> Self {
        Self::new_fdt_with_payload(phys, None)
    }

    /// Создать `BootInfo` с FDT и optional userland blob.
    pub const fn new_fdt_with_payload(
        phys: PhysicalAddress,
        userland_blob: Option<BootPayloadRange>,
    ) -> Self {
        Self {
            hw_description: HwDescription::Fdt(FdtBlob { phys }),
            userland_blob,
        }
    }
}

/// Диапазон boot-time payload в физической памяти.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BootPayloadRange {
    start: PhysicalAddress,
    size_bytes: usize,
}

impl BootPayloadRange {
    pub fn new(start: PhysicalAddress, size_bytes: usize) -> Option<Self> {
        if size_bytes == 0 || start.checked_add(size_bytes).is_none() {
            return None;
        }

        Some(Self { start, size_bytes })
    }

    pub const fn start(self) -> PhysicalAddress {
        self.start
    }

    pub const fn size_bytes(self) -> usize {
        self.size_bytes
    }

    pub fn end_exclusive(self) -> PhysicalAddress {
        self.start
            .checked_add(self.size_bytes)
            .expect("range validated in constructor")
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

    use super::{BootInfo, BootPayloadRange, HwDescription};
    use crate::test_util::build_chosen_dtb;

    #[test]
    fn new_fdt_builds_expected_variant() {
        let phys = PhysicalAddress::new(0x4000_0000);
        let info = BootInfo::new_fdt(phys);
        let HwDescription::Fdt(blob) = info.hw_description else {
            panic!("expected Fdt variant");
        };
        assert_eq!(blob.phys.as_usize(), 0x4000_0000);
        assert!(info.userland_blob.is_none());
    }

    #[test]
    fn new_fdt_with_payload_preserves_userland_blob() {
        let phys = PhysicalAddress::new(0x4000_0000);
        let payload = BootPayloadRange::new(PhysicalAddress::new(0x4800_0000), 0x20_0000).unwrap();
        let info = BootInfo::new_fdt_with_payload(phys, Some(payload));

        assert_eq!(info.userland_blob.unwrap().start().as_usize(), 0x4800_0000);
        assert_eq!(info.userland_blob.unwrap().size_bytes(), 0x20_0000);
    }

    #[test]
    fn from_fdt_extracts_runtime_initrd_range() {
        let dtb = build_chosen_dtb(Some((0x4800_0000, 0x4820_0000)));
        let info = BootInfo::from_fdt(PhysicalAddress::new(dtb.as_ptr() as usize));

        let HwDescription::Fdt(blob) = info.hw_description else {
            panic!("expected Fdt variant");
        };
        assert_eq!(blob.phys.as_usize(), dtb.as_ptr() as usize);

        let payload = info.userland_blob.expect("expected initrd payload");
        assert_eq!(payload.start().as_usize(), 0x4800_0000);
        assert_eq!(payload.size_bytes(), 0x20_0000);
    }

    #[test]
    fn payload_range_rejects_zero_sized_and_overflowing_ranges() {
        assert!(BootPayloadRange::new(PhysicalAddress::new(0x1000), 0).is_none());
        assert!(BootPayloadRange::new(PhysicalAddress::new(usize::MAX), 1).is_none());
    }

    #[test]
    fn payload_range_reports_end_exclusive() {
        let payload = BootPayloadRange::new(PhysicalAddress::new(0x8000), 0x3000).unwrap();

        assert_eq!(payload.end_exclusive().as_usize(), 0xb000);
    }
}
