use crate::startup::COMPONENT_SLOT_MAX;

/// Имя ELF-секции, в которой программа компонента несёт свой манифест.
pub const COMPONENT_MANIFEST_SECTION: &str = ".component.manifest";

/// Магия записи манифеста компонента.
pub const COMPONENT_MANIFEST_MAGIC: [u8; 8] = *b"CMPMFST\0";

/// Версия формата записи манифеста.
pub const COMPONENT_MANIFEST_VERSION: u16 = 1;

/// Размер заголовка записи манифеста в байтах.
pub const COMPONENT_MANIFEST_HEADER_SIZE: usize = 16;

const OFFSET_VERSION: usize = 8;
const OFFSET_SLOT_COUNT: usize = 10;
const OFFSET_CONFIG: usize = 12;
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
}

/// Манифест компонента, разобранный из секции его программы: виды слотов в
/// порядке приёма и признак записи конфигурации.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComponentManifest {
    slots: [SlotKind; COMPONENT_SLOT_MAX],
    slot_count: usize,
    config: bool,
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
            _ => None,
        }
    }
}

impl ComponentManifest {
    /// Виды слотов в порядке приёма.
    #[must_use]
    pub fn slots(&self) -> &[SlotKind] {
        &self.slots[..self.slot_count]
    }

    /// Компонент читает запись конфигурации.
    #[must_use]
    pub const fn config(&self) -> bool {
        self.config
    }
}

/// Кодирует запись манифеста целиком. `LEN` равна заголовку вместе с видами
/// слотов.
#[must_use]
pub const fn encode_manifest<const LEN: usize>(slots: &[SlotKind], config: bool) -> [u8; LEN] {
    assert!(
        slots.len() <= COMPONENT_SLOT_MAX,
        "slot count fits the frame"
    );
    assert!(
        LEN == COMPONENT_MANIFEST_HEADER_SIZE + slots.len(),
        "record length covers the header and every slot"
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
    bytes[OFFSET_CONFIG] = if config { 1 } else { 0 };

    let mut slot = 0;
    while slot < slots.len() {
        bytes[COMPONENT_MANIFEST_HEADER_SIZE + slot] = slots[slot] as u8;
        slot += 1;
    }
    bytes
}

/// Разбирает запись манифеста. Буфер разбирается целиком: хвост сверх записи
/// отвергается.
pub fn decode_manifest(bytes: &[u8]) -> Result<ComponentManifest, ManifestDecodeError> {
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

    let config = match bytes[OFFSET_CONFIG] {
        0 => false,
        1 => true,
        _ => return Err(ManifestDecodeError::InvalidLayout("config")),
    };
    let slot_count = usize::from(read_u16(bytes, OFFSET_SLOT_COUNT));
    if slot_count > COMPONENT_SLOT_MAX {
        return Err(ManifestDecodeError::InvalidLayout("slot count"));
    }
    let needed = COMPONENT_MANIFEST_HEADER_SIZE + slot_count;
    if bytes.len() != needed {
        return Err(ManifestDecodeError::Truncated {
            needed,
            actual: bytes.len(),
        });
    }

    let mut slots = [SlotKind::Image; COMPONENT_SLOT_MAX];
    for (slot, byte) in slots
        .iter_mut()
        .zip(&bytes[COMPONENT_MANIFEST_HEADER_SIZE..needed])
    {
        *slot =
            SlotKind::from_byte(*byte).ok_or(ManifestDecodeError::InvalidLayout("slot kind"))?;
    }
    Ok(ComponentManifest {
        slots,
        slot_count,
        config,
    })
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECORD_LEN: usize = COMPONENT_MANIFEST_HEADER_SIZE + 2;

    fn device_record() -> [u8; RECORD_LEN] {
        encode_manifest([SlotKind::Irq, SlotKind::Mmio].as_slice(), false)
    }

    #[test]
    fn a_record_carries_every_slot_kind_in_order() {
        let manifest = decode_manifest(&device_record()).expect("record decodes");

        assert_eq!(manifest.slots(), [SlotKind::Irq, SlotKind::Mmio]);
        assert!(!manifest.config());
    }

    #[test]
    fn a_record_without_slots_carries_the_config_flag() {
        let bytes: [u8; COMPONENT_MANIFEST_HEADER_SIZE] = encode_manifest(&[], true);

        let manifest = decode_manifest(&bytes).expect("record decodes");
        assert_eq!(manifest.slots(), []);
        assert!(manifest.config());
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

        assert_eq!(
            decode_manifest(&bytes),
            Err(ManifestDecodeError::Truncated {
                needed: RECORD_LEN,
                actual: RECORD_LEN + 1,
            })
        );
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
    fn a_non_zero_reserved_field_is_refused() {
        let mut bytes = device_record();
        bytes[OFFSET_RESERVED] = 1;

        assert_eq!(
            decode_manifest(&bytes),
            Err(ManifestDecodeError::InvalidLayout("reserved"))
        );
    }
}
