use crate::startup::COMPONENT_SLOT_MAX;

/// Имя ELF-секции, в которой программа компонента несёт свой манифест.
pub const COMPONENT_MANIFEST_SECTION: &str = ".component.manifest";

/// Магия записи манифеста компонента.
pub const COMPONENT_MANIFEST_MAGIC: [u8; 8] = *b"CMPMFST\0";

/// Версия формата записи манифеста.
pub const COMPONENT_MANIFEST_VERSION: u16 = 2;

/// Размер заголовка записи манифеста в байтах.
pub const COMPONENT_MANIFEST_HEADER_SIZE: usize = 16;

/// Размер заголовка записи слота в байтах.
pub const COMPONENT_SLOT_HEADER_SIZE: usize = 4;

const OFFSET_VERSION: usize = 8;
const OFFSET_SLOT_COUNT: usize = 10;
const OFFSET_EXPORT_LEN: usize = 12;
const OFFSET_RESERVED: usize = 13;

/// Вид capability, которую компонент принимает слотом стартовой транзакции.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotKind {
    /// Регион userland-образа.
    Image = 0,
    /// Линия прерывания устройства.
    Irq = 1,
    /// MMIO-окно устройства.
    Mmio = 2,
    /// Сессия service-порта другого компонента.
    Service = 3,
    /// Регион записи образа из каталога компонента.
    File = 4,
}

/// Слот, объявленный программой: вид, номер ресурса устройства и имена, по
/// которым композиция ищет источник - строки совместимости у устройства и имя
/// протокола у сервиса.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotDeclaration {
    kind: SlotKind,
    resource: u16,
    names: &'static [&'static str],
}

/// Слот записи манифеста.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManifestSlot<'a> {
    kind: SlotKind,
    resource: u16,
    names: &'a [u8],
    name_count: u8,
}

/// Имена слота в порядке объявления.
#[derive(Debug, Clone)]
pub struct SlotNames<'a> {
    bytes: &'a [u8],
    cursor: usize,
    remaining: u8,
}

/// Манифест компонента, разобранный из секции его программы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComponentManifest<'a> {
    slots: &'a [u8],
    slot_count: u16,
    export: Option<&'a str>,
}

/// Слоты манифеста в порядке приёма.
#[derive(Debug, Clone)]
pub struct ManifestSlots<'a> {
    bytes: &'a [u8],
    cursor: usize,
    remaining: u16,
}

/// Причина, по которой байты секции манифестом не являются.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestDecodeError {
    /// Запись начата чужой магией.
    InvalidMagic([u8; 8]),
    /// Версия формата записи не поддерживается.
    InvalidVersion(u16),
    /// Байт меньше, чем требует запись.
    Truncated { needed: usize, actual: usize },
    /// Поле записи вне множества своих значений.
    InvalidLayout(&'static str),
}

impl SlotKind {
    const fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::Image),
            1 => Some(Self::Irq),
            2 => Some(Self::Mmio),
            3 => Some(Self::Service),
            4 => Some(Self::File),
            _ => None,
        }
    }

    const fn names_device(self) -> bool {
        matches!(self, Self::Irq | Self::Mmio)
    }
}

impl SlotDeclaration {
    /// Регион userland-образа.
    #[must_use]
    pub const fn image() -> Self {
        Self {
            kind: SlotKind::Image,
            resource: 0,
            names: &[],
        }
    }

    /// Линия прерывания устройства, совместимого с одной из `compatible`.
    /// `resource` называет линию среди линий устройства.
    #[must_use]
    pub const fn irq(compatible: &'static [&'static str], resource: u16) -> Self {
        Self {
            kind: SlotKind::Irq,
            resource,
            names: compatible,
        }
    }

    /// MMIO-окно устройства, совместимого с одной из `compatible`. `resource`
    /// называет окно среди окон устройства.
    #[must_use]
    pub const fn mmio(compatible: &'static [&'static str], resource: u16) -> Self {
        Self {
            kind: SlotKind::Mmio,
            resource,
            names: compatible,
        }
    }

    /// Сессия компонента, экспортирующего протокол `protocol`.
    #[must_use]
    pub const fn service(protocol: &'static [&'static str]) -> Self {
        Self {
            kind: SlotKind::Service,
            resource: 0,
            names: protocol,
        }
    }

    /// Запись из каталога компонента, названная относительным именем.
    #[must_use]
    pub const fn file(name: &'static [&'static str]) -> Self {
        Self {
            kind: SlotKind::File,
            resource: 0,
            names: name,
        }
    }
}

impl<'a> ManifestSlot<'a> {
    /// Вид capability слота.
    #[must_use]
    pub const fn kind(&self) -> SlotKind {
        self.kind
    }

    /// Номер ресурса устройства: линии либо окна. У прочих слотов ноль.
    #[must_use]
    pub const fn resource(&self) -> u16 {
        self.resource
    }

    /// Имена слота: строки совместимости устройства либо имя протокола.
    #[must_use]
    pub const fn names(&self) -> SlotNames<'a> {
        SlotNames {
            bytes: self.names,
            cursor: 0,
            remaining: self.name_count,
        }
    }
}

impl<'a> ComponentManifest<'a> {
    /// Слоты в порядке приёма.
    #[must_use]
    pub const fn slots(&self) -> ManifestSlots<'a> {
        ManifestSlots {
            bytes: self.slots,
            cursor: 0,
            remaining: self.slot_count,
        }
    }

    /// Число слотов стартовой транзакции.
    #[must_use]
    pub const fn slot_count(&self) -> u16 {
        self.slot_count
    }

    /// Протокол, который компонент обслуживает на своём service-порту.
    #[must_use]
    pub const fn export(&self) -> Option<&'a str> {
        self.export
    }
}

impl<'a> Iterator for ManifestSlots<'a> {
    type Item = ManifestSlot<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let parsed = parse_slot(self.bytes, self.cursor).ok()?;
        self.remaining -= 1;
        self.cursor = parsed.end;
        Some(parsed.slot)
    }
}

impl<'a> Iterator for SlotNames<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let len = usize::from(*self.bytes.get(self.cursor)?);
        let at = self.cursor + 1;
        let name = core::str::from_utf8(self.bytes.get(at..at + len)?).ok()?;
        self.remaining -= 1;
        self.cursor = at + len;
        Some(name)
    }
}

/// Длина записи манифеста с заголовком, экспортом и слотами.
#[must_use]
pub const fn manifest_len(slots: &[SlotDeclaration], export: &str) -> usize {
    let mut len = COMPONENT_MANIFEST_HEADER_SIZE + export.len();
    let mut index = 0;
    while index < slots.len() {
        len += COMPONENT_SLOT_HEADER_SIZE;
        let names = slots[index].names;
        let mut name = 0;
        while name < names.len() {
            len += 1 + names[name].len();
            name += 1;
        }
        index += 1;
    }
    len
}

/// Кодирует запись манифеста целиком. `LEN` равна [`manifest_len`].
#[must_use]
pub const fn encode_manifest<const LEN: usize>(
    slots: &[SlotDeclaration],
    export: &str,
) -> [u8; LEN] {
    assert!(
        slots.len() <= COMPONENT_SLOT_MAX,
        "slot count fits the frame"
    );
    assert!(
        LEN == manifest_len(slots, export),
        "record length covers the header, the export and every slot"
    );
    assert!(
        export.len() <= u8::MAX as usize,
        "export name fits its field"
    );

    let mut bytes = [0; LEN];
    let mut index = 0;
    while index < COMPONENT_MANIFEST_MAGIC.len() {
        bytes[index] = COMPONENT_MANIFEST_MAGIC[index];
        index += 1;
    }
    let version = COMPONENT_MANIFEST_VERSION.to_le_bytes();
    bytes[OFFSET_VERSION] = version[0];
    bytes[OFFSET_VERSION + 1] = version[1];
    let count = (slots.len() as u16).to_le_bytes();
    bytes[OFFSET_SLOT_COUNT] = count[0];
    bytes[OFFSET_SLOT_COUNT + 1] = count[1];
    bytes[OFFSET_EXPORT_LEN] = export.len() as u8;

    let mut at = COMPONENT_MANIFEST_HEADER_SIZE;
    at = write_bytes(&mut bytes, at, export.as_bytes());

    let mut slot = 0;
    while slot < slots.len() {
        let declaration = &slots[slot];
        bytes[at] = declaration.kind as u8;
        let resource = declaration.resource.to_le_bytes();
        bytes[at + 1] = resource[0];
        bytes[at + 2] = resource[1];
        bytes[at + 3] = declaration.names.len() as u8;
        at += COMPONENT_SLOT_HEADER_SIZE;

        let mut name = 0;
        while name < declaration.names.len() {
            let text = declaration.names[name].as_bytes();
            assert!(!text.is_empty(), "slot name is non-empty");
            assert!(text.len() <= u8::MAX as usize, "slot name fits its field");
            bytes[at] = text.len() as u8;
            at = write_bytes(&mut bytes, at + 1, text);
            name += 1;
        }
        slot += 1;
    }
    bytes
}

const fn write_bytes<const LEN: usize>(bytes: &mut [u8; LEN], at: usize, text: &[u8]) -> usize {
    let mut index = 0;
    while index < text.len() {
        bytes[at + index] = text[index];
        index += 1;
    }
    at + text.len()
}

/// Разбирает запись манифеста. Буфер разбирается целиком: хвост сверх записи
/// отвергается.
pub fn decode_manifest(bytes: &[u8]) -> Result<ComponentManifest<'_>, ManifestDecodeError> {
    if bytes.len() < COMPONENT_MANIFEST_HEADER_SIZE {
        return Err(ManifestDecodeError::Truncated {
            needed: COMPONENT_MANIFEST_HEADER_SIZE,
            actual: bytes.len(),
        });
    }
    let magic: [u8; 8] = bytes[..COMPONENT_MANIFEST_MAGIC.len()]
        .try_into()
        .map_err(|_| ManifestDecodeError::InvalidLayout("magic"))?;
    if magic != COMPONENT_MANIFEST_MAGIC {
        return Err(ManifestDecodeError::InvalidMagic(magic));
    }
    let version = read_u16(bytes, OFFSET_VERSION);
    if version != COMPONENT_MANIFEST_VERSION {
        return Err(ManifestDecodeError::InvalidVersion(version));
    }
    if bytes[OFFSET_RESERVED..COMPONENT_MANIFEST_HEADER_SIZE]
        .iter()
        .any(|byte| *byte != 0)
    {
        return Err(ManifestDecodeError::InvalidLayout("reserved"));
    }

    let slot_count = read_u16(bytes, OFFSET_SLOT_COUNT);
    if usize::from(slot_count) > COMPONENT_SLOT_MAX {
        return Err(ManifestDecodeError::InvalidLayout("slot count"));
    }

    let export_len = usize::from(bytes[OFFSET_EXPORT_LEN]);
    let export_end = COMPONENT_MANIFEST_HEADER_SIZE + export_len;
    let export = bytes
        .get(COMPONENT_MANIFEST_HEADER_SIZE..export_end)
        .ok_or(ManifestDecodeError::Truncated {
            needed: export_end,
            actual: bytes.len(),
        })?;
    let export =
        core::str::from_utf8(export).map_err(|_| ManifestDecodeError::InvalidLayout("export"))?;

    let mut cursor = export_end;
    for _ in 0..slot_count {
        cursor = parse_slot(bytes, cursor)?.end;
    }
    if cursor != bytes.len() {
        return Err(ManifestDecodeError::Truncated {
            needed: cursor,
            actual: bytes.len(),
        });
    }

    Ok(ComponentManifest {
        slots: bytes
            .get(export_end..cursor)
            .ok_or(ManifestDecodeError::InvalidLayout("slots"))?,
        slot_count,
        export: (!export.is_empty()).then_some(export),
    })
}

struct ParsedSlot<'a> {
    slot: ManifestSlot<'a>,
    end: usize,
}

fn parse_slot(bytes: &[u8], at: usize) -> Result<ParsedSlot<'_>, ManifestDecodeError> {
    let header =
        bytes
            .get(at..at + COMPONENT_SLOT_HEADER_SIZE)
            .ok_or(ManifestDecodeError::Truncated {
                needed: at + COMPONENT_SLOT_HEADER_SIZE,
                actual: bytes.len(),
            })?;
    let kind =
        SlotKind::from_byte(header[0]).ok_or(ManifestDecodeError::InvalidLayout("slot kind"))?;
    let resource = u16::from_le_bytes([header[1], header[2]]);
    let name_count = header[3];

    // Устройство названо хотя бы одной совместимостью, сервис - своим
    // протоколом, а образ именами не выбирается.
    match kind {
        SlotKind::Image if name_count != 0 || resource != 0 => {
            return Err(ManifestDecodeError::InvalidLayout("image slot"));
        }
        SlotKind::Service if name_count != 1 || resource != 0 => {
            return Err(ManifestDecodeError::InvalidLayout("service slot"));
        }
        SlotKind::File if name_count != 1 || resource != 0 => {
            return Err(ManifestDecodeError::InvalidLayout("file slot"));
        }
        kind if kind.names_device() && name_count == 0 => {
            return Err(ManifestDecodeError::InvalidLayout("device slot"));
        }
        _ => {}
    }

    let names_at = at + COMPONENT_SLOT_HEADER_SIZE;
    let mut cursor = names_at;
    for _ in 0..name_count {
        let len = usize::from(*bytes.get(cursor).ok_or(ManifestDecodeError::Truncated {
            needed: cursor + 1,
            actual: bytes.len(),
        })?);
        if len == 0 {
            return Err(ManifestDecodeError::InvalidLayout("slot name"));
        }
        let end = cursor + 1 + len;
        let name = bytes
            .get(cursor + 1..end)
            .ok_or(ManifestDecodeError::Truncated {
                needed: end,
                actual: bytes.len(),
            })?;
        core::str::from_utf8(name).map_err(|_| ManifestDecodeError::InvalidLayout("slot name"))?;
        cursor = end;
    }

    Ok(ParsedSlot {
        slot: ManifestSlot {
            kind,
            resource,
            names: bytes
                .get(names_at..cursor)
                .ok_or(ManifestDecodeError::InvalidLayout("slot names"))?,
            name_count,
        },
        end: cursor,
    })
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

#[cfg(test)]
mod tests {
    use std::vec::Vec;

    use super::*;

    const RTC: &[&str] = &["arm,pl031", "arm,primecell"];
    const NAMESPACE: &[&str] = &["Namespace"];

    const DEVICE_SLOTS: &[SlotDeclaration] =
        &[SlotDeclaration::irq(RTC, 0), SlotDeclaration::mmio(RTC, 1)];
    const DEVICE_LEN: usize = manifest_len(DEVICE_SLOTS, "");

    fn device_record() -> [u8; DEVICE_LEN] {
        encode_manifest(DEVICE_SLOTS, "")
    }

    fn names<'a>(slot: &ManifestSlot<'a>) -> Vec<&'a str> {
        slot.names().collect()
    }

    #[test]
    fn a_record_carries_every_slot_with_its_names() {
        let bytes = device_record();
        let manifest = decode_manifest(&bytes).expect("record decodes");

        let slots: Vec<ManifestSlot<'_>> = manifest.slots().collect();
        assert_eq!(slots.len(), 2);
        assert_eq!(slots[0].kind(), SlotKind::Irq);
        assert_eq!(slots[0].resource(), 0);
        assert_eq!(names(&slots[0]), RTC);
        assert_eq!(slots[1].kind(), SlotKind::Mmio);
        assert_eq!(slots[1].resource(), 1);
        assert_eq!(names(&slots[1]), RTC);
        assert_eq!(manifest.export(), None);
    }

    #[test]
    fn a_record_carries_the_exported_protocol() {
        const SLOTS: &[SlotDeclaration] = &[SlotDeclaration::image()];
        const LEN: usize = manifest_len(SLOTS, "Namespace");
        let bytes: [u8; LEN] = encode_manifest(SLOTS, "Namespace");

        let manifest = decode_manifest(&bytes).expect("record decodes");
        assert_eq!(manifest.export(), Some("Namespace"));
        let slots: Vec<ManifestSlot<'_>> = manifest.slots().collect();
        assert_eq!(slots[0].kind(), SlotKind::Image);
        assert_eq!(names(&slots[0]), Vec::<&str>::new());
    }

    #[test]
    fn a_file_slot_carries_its_name() {
        const SLOTS: &[SlotDeclaration] = &[SlotDeclaration::file(&["mounts"])];
        const LEN: usize = manifest_len(SLOTS, "");
        let bytes: [u8; LEN] = encode_manifest(SLOTS, "");

        let manifest = decode_manifest(&bytes).expect("record decodes");
        let slots: Vec<ManifestSlot<'_>> = manifest.slots().collect();
        assert_eq!(slots[0].kind(), SlotKind::File);
        assert_eq!(names(&slots[0]), ["mounts"]);
    }

    #[test]
    fn a_service_slot_carries_its_protocol() {
        const SLOTS: &[SlotDeclaration] = &[SlotDeclaration::service(NAMESPACE)];
        const LEN: usize = manifest_len(SLOTS, "");
        let bytes: [u8; LEN] = encode_manifest(SLOTS, "");

        let manifest = decode_manifest(&bytes).expect("record decodes");
        let slots: Vec<ManifestSlot<'_>> = manifest.slots().collect();
        assert_eq!(slots[0].kind(), SlotKind::Service);
        assert_eq!(names(&slots[0]), ["Namespace"]);
    }

    #[test]
    fn a_record_without_slots_decodes() {
        const LEN: usize = manifest_len(&[], "");
        let bytes: [u8; LEN] = encode_manifest(&[], "");

        let manifest = decode_manifest(&bytes).expect("record decodes");
        assert_eq!(manifest.slots().count(), 0);
        assert_eq!(manifest.slot_count(), 0);
    }

    #[test]
    fn a_foreign_magic_is_refused() {
        let mut bytes = device_record();
        bytes[0] = b'X';

        assert_eq!(
            decode_manifest(&bytes),
            Err(ManifestDecodeError::InvalidMagic([
                b'X', b'M', b'P', b'M', b'F', b'S', b'T', 0
            ]))
        );
    }

    #[test]
    fn another_version_is_refused() {
        let mut bytes = device_record();
        bytes[OFFSET_VERSION] = 9;

        assert_eq!(
            decode_manifest(&bytes),
            Err(ManifestDecodeError::InvalidVersion(9))
        );
    }

    #[test]
    fn bytes_past_the_record_are_refused() {
        let mut bytes = device_record().to_vec();
        bytes.push(0);

        assert!(matches!(
            decode_manifest(&bytes),
            Err(ManifestDecodeError::Truncated { .. })
        ));
    }

    #[test]
    fn an_unknown_slot_kind_is_refused() {
        let mut bytes = device_record();
        bytes[COMPONENT_MANIFEST_HEADER_SIZE] = 9;

        assert_eq!(
            decode_manifest(&bytes),
            Err(ManifestDecodeError::InvalidLayout("slot kind"))
        );
    }

    #[test]
    fn a_device_slot_without_names_is_refused() {
        let mut bytes = device_record();
        bytes[COMPONENT_MANIFEST_HEADER_SIZE + 3] = 0;

        assert_eq!(
            decode_manifest(&bytes),
            Err(ManifestDecodeError::InvalidLayout("device slot"))
        );
    }

    #[test]
    fn a_service_slot_past_one_protocol_is_refused() {
        const SLOTS: &[SlotDeclaration] = &[SlotDeclaration::service(NAMESPACE)];
        const LEN: usize = manifest_len(SLOTS, "");
        let mut bytes: [u8; LEN] = encode_manifest(SLOTS, "");
        bytes[COMPONENT_MANIFEST_HEADER_SIZE + 3] = 2;

        assert_eq!(
            decode_manifest(&bytes),
            Err(ManifestDecodeError::InvalidLayout("service slot"))
        );
    }

    #[test]
    fn an_image_slot_with_a_resource_is_refused() {
        const SLOTS: &[SlotDeclaration] = &[SlotDeclaration::image()];
        const LEN: usize = manifest_len(SLOTS, "");
        let mut bytes: [u8; LEN] = encode_manifest(SLOTS, "");
        bytes[COMPONENT_MANIFEST_HEADER_SIZE + 1] = 1;

        assert_eq!(
            decode_manifest(&bytes),
            Err(ManifestDecodeError::InvalidLayout("image slot"))
        );
    }

    #[test]
    fn an_empty_slot_name_is_refused() {
        let mut bytes = device_record();
        bytes[COMPONENT_MANIFEST_HEADER_SIZE + COMPONENT_SLOT_HEADER_SIZE] = 0;

        assert_eq!(
            decode_manifest(&bytes),
            Err(ManifestDecodeError::InvalidLayout("slot name"))
        );
    }

    #[test]
    fn a_non_zero_reserved_field_is_refused() {
        let mut bytes = device_record();
        bytes[OFFSET_RESERVED] = 1;

        assert_eq!(
            decode_manifest(&bytes),
            Err(ManifestDecodeError::InvalidLayout("reserved"))
        );
    }

    #[test]
    fn a_truncated_record_is_refused() {
        let bytes = device_record();
        for length in [0, COMPONENT_MANIFEST_HEADER_SIZE, bytes.len() - 1] {
            assert!(
                matches!(
                    decode_manifest(&bytes[..length]),
                    Err(ManifestDecodeError::Truncated { .. })
                ),
                "{length} bytes must not decode"
            );
        }
    }
}
