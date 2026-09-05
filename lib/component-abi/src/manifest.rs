use crate::startup::COMPONENT_SLOT_MAX;

/// Имя ELF-секции, в которой программа компонента несёт свой манифест.
pub const COMPONENT_MANIFEST_SECTION: &str = ".component.manifest";

/// Магия записи манифеста компонента.
pub const COMPONENT_MANIFEST_MAGIC: [u8; 8] = *b"CMPMFST\0";

/// Версия формата записи манифеста.
pub const COMPONENT_MANIFEST_VERSION: u16 = 1;

/// Размер заголовка записи манифеста в байтах.
pub const COMPONENT_MANIFEST_HEADER_SIZE: usize = 16;

/// Размер заголовка записи слота в байтах.
pub const COMPONENT_SLOT_HEADER_SIZE: usize = 4;

const OFFSET_VERSION: usize = 8;
const OFFSET_SLOT_COUNT: usize = 10;
const OFFSET_EXPORT_LEN: usize = 12;
const OFFSET_FLAGS: usize = 13;
const OFFSET_CONTROL_LEN: usize = 14;
const OFFSET_RESERVED: usize = 15;

const FLAG_ESTABLISHES_SESSIONS: u8 = 1;

const PROTOCOL_NAME_MAX: u8 = 2;

/// Вид capability, которую компонент принимает слотом стартовой транзакции.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotKind {
    /// Read-only регион userland-образа.
    Userland = 0,
    /// Линия прерывания устройства.
    Irq = 1,
    /// MMIO-окно устройства.
    Mmio = 2,
    /// Сессия service-порта другого компонента.
    Service = 3,
    /// Сессия другого компонента, суженная до каталога потребителя.
    Directory = 4,
    /// Клиентская capability supervisor, поднявшего компонент.
    Supervisor = 5,
    /// Клиентская capability control-порта другого компонента.
    Control = 6,
}

/// Слот, объявленный программой: вид, номер ресурса устройства и имена, по
/// которым композиция ищет источник - строки совместимости у устройства, имя
/// протокола и необязательное имя экземпляра у импорта.
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
    control: Option<&'a str>,
    establishes_sessions: bool,
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
            0 => Some(Self::Userland),
            1 => Some(Self::Irq),
            2 => Some(Self::Mmio),
            3 => Some(Self::Service),
            4 => Some(Self::Directory),
            5 => Some(Self::Supervisor),
            6 => Some(Self::Control),
            _ => None,
        }
    }

    const fn names_device(self) -> bool {
        matches!(self, Self::Irq | Self::Mmio)
    }
}

impl SlotDeclaration {
    /// Read-only регион userland-образа.
    #[must_use]
    pub const fn userland() -> Self {
        Self {
            kind: SlotKind::Userland,
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

    /// Сессия компонента, экспортирующего протокол. `protocol` несёт имя
    /// протокола, а за ним - необязательное имя экземпляра-экспортёра.
    #[must_use]
    pub const fn service(protocol: &'static [&'static str]) -> Self {
        Self {
            kind: SlotKind::Service,
            resource: 0,
            names: protocol,
        }
    }

    /// Сессия экземпляра `provider`, суженная до каталога компонента. Путь каталога
    /// назначает композиция, и объявление называет только провайдера.
    #[must_use]
    pub const fn directory(provider: &'static [&'static str]) -> Self {
        Self {
            kind: SlotKind::Directory,
            resource: 0,
            names: provider,
        }
    }

    /// Клиентская capability supervisor, поднявшего компонент.
    #[must_use]
    pub const fn supervisor() -> Self {
        Self {
            kind: SlotKind::Supervisor,
            resource: 0,
            names: &[],
        }
    }

    /// Клиентская capability control-порта компонента, обслуживающего протокол.
    /// `protocol` несёт имя протокола, а за ним - необязательное имя
    /// экземпляра-провайдера.
    #[must_use]
    pub const fn control(protocol: &'static [&'static str]) -> Self {
        Self {
            kind: SlotKind::Control,
            resource: 0,
            names: protocol,
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

    /// Протокол, который компонент обслуживает на своём control-порту.
    #[must_use]
    pub const fn control(&self) -> Option<&'a str> {
        self.control
    }

    /// Компонент устанавливает сессии своего service-порта сам: композиция просит их
    /// операцией `ComponentSessions::open_session` на его control-порту.
    #[must_use]
    pub const fn establishes_sessions(&self) -> bool {
        self.establishes_sessions
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

/// Длина записи манифеста с заголовком, обслуживаемыми протоколами и слотами.
#[must_use]
pub const fn manifest_len(slots: &[SlotDeclaration], export: &str, control: &str) -> usize {
    let mut len = COMPONENT_MANIFEST_HEADER_SIZE + export.len() + control.len();
    let mut pending = slots;
    while let [slot, tail @ ..] = pending {
        len += COMPONENT_SLOT_HEADER_SIZE;
        let mut names = slot.names;
        while let [name, others @ ..] = names {
            len += 1 + name.len();
            names = others;
        }
        pending = tail;
    }
    len
}

/// Кодирует запись манифеста целиком. `LEN` равна [`manifest_len`]. `export` и `control`
/// называют протоколы service- и control-порта, а `establishes_sessions` объявляет, что
/// сессии своего service-порта компонент заводит сам.
#[must_use]
pub const fn encode_manifest<const LEN: usize>(
    slots: &[SlotDeclaration],
    export: &str,
    control: &str,
    establishes_sessions: bool,
) -> [u8; LEN] {
    assert!(
        slots.len() <= COMPONENT_SLOT_MAX,
        "slot count fits the frame"
    );
    assert!(
        LEN == manifest_len(slots, export, control),
        "record length covers the header, the served protocols and every slot"
    );

    let mut header = [0; COMPONENT_MANIFEST_HEADER_SIZE];
    let (magic, _) = header.split_at_mut(COMPONENT_MANIFEST_MAGIC.len());
    magic.copy_from_slice(&COMPONENT_MANIFEST_MAGIC);
    let version = COMPONENT_MANIFEST_VERSION.to_le_bytes();
    header[OFFSET_VERSION] = version[0];
    header[OFFSET_VERSION + 1] = version[1];
    // Число слотов ограничено `COMPONENT_SLOT_MAX` проверкой выше.
    #[allow(clippy::cast_possible_truncation)]
    let count = (slots.len() as u16).to_le_bytes();
    header[OFFSET_SLOT_COUNT] = count[0];
    header[OFFSET_SLOT_COUNT + 1] = count[1];
    header[OFFSET_EXPORT_LEN] = len_byte(export.len());
    header[OFFSET_CONTROL_LEN] = len_byte(control.len());
    if establishes_sessions {
        header[OFFSET_FLAGS] = FLAG_ESTABLISHES_SESSIONS;
    }

    let mut bytes = [0; LEN];
    let mut rest: &mut [u8] = &mut bytes;
    rest = write_bytes(rest, &header);
    rest = write_bytes(rest, export.as_bytes());
    rest = write_bytes(rest, control.as_bytes());

    let mut pending = slots;
    while let [declaration, tail @ ..] = pending {
        let resource = declaration.resource.to_le_bytes();
        let slot_header = [
            declaration.kind as u8,
            resource[0],
            resource[1],
            len_byte(declaration.names.len()),
        ];
        rest = write_bytes(rest, &slot_header);

        let mut names = declaration.names;
        while let [name, others @ ..] = names {
            let text = name.as_bytes();
            assert!(!text.is_empty(), "slot name is non-empty");
            rest = write_bytes(rest, &[len_byte(text.len())]);
            rest = write_bytes(rest, text);
            names = others;
        }
        pending = tail;
    }
    bytes
}

#[allow(clippy::cast_possible_truncation)]
const fn len_byte(len: usize) -> u8 {
    assert!(len <= u8::MAX as usize, "field length fits a byte");
    len as u8
}

const fn write_bytes<'a>(bytes: &'a mut [u8], text: &[u8]) -> &'a mut [u8] {
    // `LEN` равна `manifest_len`, и остаток буфера покрывает `text`.
    let (written, rest) = bytes.split_at_mut(text.len());
    written.copy_from_slice(text);
    rest
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
    let magic = *bytes
        .first_chunk::<8>()
        .ok_or(ManifestDecodeError::InvalidLayout("magic"))?;
    if magic != COMPONENT_MANIFEST_MAGIC {
        return Err(ManifestDecodeError::InvalidMagic(magic));
    }
    let version = read_u16(bytes, OFFSET_VERSION);
    if version != COMPONENT_MANIFEST_VERSION {
        return Err(ManifestDecodeError::InvalidVersion(version));
    }
    let reserved = bytes
        .get(OFFSET_RESERVED..COMPONENT_MANIFEST_HEADER_SIZE)
        .ok_or(ManifestDecodeError::InvalidLayout("reserved"))?;
    if reserved.iter().any(|byte| *byte != 0) {
        return Err(ManifestDecodeError::InvalidLayout("reserved"));
    }

    let flags = *bytes
        .get(OFFSET_FLAGS)
        .ok_or(ManifestDecodeError::InvalidLayout("flags"))?;
    if flags & !FLAG_ESTABLISHES_SESSIONS != 0 {
        return Err(ManifestDecodeError::InvalidLayout("flags"));
    }

    let slot_count = read_u16(bytes, OFFSET_SLOT_COUNT);
    if usize::from(slot_count) > COMPONENT_SLOT_MAX {
        return Err(ManifestDecodeError::InvalidLayout("slot count"));
    }

    let export_len = *bytes
        .get(OFFSET_EXPORT_LEN)
        .ok_or(ManifestDecodeError::InvalidLayout("export length"))?;
    let control_len = *bytes
        .get(OFFSET_CONTROL_LEN)
        .ok_or(ManifestDecodeError::InvalidLayout("control length"))?;
    let export_end = COMPONENT_MANIFEST_HEADER_SIZE + usize::from(export_len);
    let export = parse_protocol(bytes, COMPONENT_MANIFEST_HEADER_SIZE, export_end, "export")?;
    let control_end = export_end + usize::from(control_len);
    let control = parse_protocol(bytes, export_end, control_end, "control")?;

    let mut cursor = control_end;
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
            .get(control_end..cursor)
            .ok_or(ManifestDecodeError::InvalidLayout("slots"))?,
        slot_count,
        export,
        control,
        establishes_sessions: flags & FLAG_ESTABLISHES_SESSIONS != 0,
    })
}

fn parse_protocol<'a>(
    bytes: &'a [u8],
    at: usize,
    end: usize,
    field: &'static str,
) -> Result<Option<&'a str>, ManifestDecodeError> {
    let name = bytes.get(at..end).ok_or(ManifestDecodeError::Truncated {
        needed: end,
        actual: bytes.len(),
    })?;
    let name = core::str::from_utf8(name).map_err(|_| ManifestDecodeError::InvalidLayout(field))?;
    Ok((!name.is_empty()).then_some(name))
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
    let [kind, resource_low, resource_high, name_count] = *header else {
        return Err(ManifestDecodeError::InvalidLayout("slot header"));
    };
    let kind = SlotKind::from_byte(kind).ok_or(ManifestDecodeError::InvalidLayout("slot kind"))?;
    let resource = u16::from_le_bytes([resource_low, resource_high]);

    // Устройство названо хотя бы одной совместимостью, импорт протокола - его именем и
    // необязательным именем экземпляра, каталог - именем экземпляра-провайдера, а у
    // userland-образа и supervisor число имён равно нулю.
    match kind {
        SlotKind::Userland if name_count != 0 || resource != 0 => {
            return Err(ManifestDecodeError::InvalidLayout("userland slot"));
        }
        SlotKind::Directory if name_count != 1 || resource != 0 => {
            return Err(ManifestDecodeError::InvalidLayout("directory slot"));
        }
        SlotKind::Supervisor if name_count != 0 || resource != 0 => {
            return Err(ManifestDecodeError::InvalidLayout("supervisor slot"));
        }
        SlotKind::Service if name_count == 0 || name_count > PROTOCOL_NAME_MAX || resource != 0 => {
            return Err(ManifestDecodeError::InvalidLayout("service slot"));
        }
        SlotKind::Control if name_count == 0 || name_count > PROTOCOL_NAME_MAX || resource != 0 => {
            return Err(ManifestDecodeError::InvalidLayout("control slot"));
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
    // На неполном буфере поле читается нулём, и разбор падает дальше, на несовпадении
    // значения.
    bytes
        .get(offset..)
        .and_then(<[u8]>::first_chunk::<2>)
        .map_or(0, |field| u16::from_le_bytes(*field))
}

#[cfg(test)]
mod tests {
    use std::vec::Vec;

    use super::*;

    const RTC: &[&str] = &["arm,pl031", "arm,primecell"];
    const NAMESPACE: &[&str] = &["Namespace"];
    const NAMED_FILE_SYSTEM: &[&str] = &["FileSystem", "ramfs"];
    const NAMED_ADMIN: &[&str] = &["NamespaceAdmin", "namespace-router"];
    const IMAGE_FS: &[&str] = &["filesystem-imagefs"];

    const DEVICE_SLOTS: &[SlotDeclaration] =
        &[SlotDeclaration::irq(RTC, 0), SlotDeclaration::mmio(RTC, 1)];
    const DEVICE_LEN: usize = manifest_len(DEVICE_SLOTS, "", "");

    fn device_record() -> [u8; DEVICE_LEN] {
        encode_manifest(DEVICE_SLOTS, "", "", false)
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
        const SLOTS: &[SlotDeclaration] = &[SlotDeclaration::userland()];
        const LEN: usize = manifest_len(SLOTS, "Namespace", "");
        let bytes: [u8; LEN] = encode_manifest(SLOTS, "Namespace", "", false);

        let manifest = decode_manifest(&bytes).expect("record decodes");
        assert_eq!(manifest.export(), Some("Namespace"));
        let slots: Vec<ManifestSlot<'_>> = manifest.slots().collect();
        assert_eq!(slots[0].kind(), SlotKind::Userland);
        assert_eq!(names(&slots[0]), Vec::<&str>::new());
    }

    #[test]
    fn a_record_carries_the_declaration_of_own_sessions() {
        const SLOTS: &[SlotDeclaration] = &[SlotDeclaration::userland()];
        const LEN: usize = manifest_len(SLOTS, "FileSystem", "");
        let bytes: [u8; LEN] = encode_manifest(SLOTS, "FileSystem", "", true);

        let manifest = decode_manifest(&bytes).expect("record decodes");
        assert!(manifest.establishes_sessions());
        assert_eq!(manifest.export(), Some("FileSystem"));

        let minted: [u8; LEN] = encode_manifest(SLOTS, "FileSystem", "", false);
        assert!(
            !decode_manifest(&minted)
                .expect("record decodes")
                .establishes_sessions()
        );
    }

    #[test]
    fn a_record_carries_the_protocols_of_both_ports_apart() {
        const SLOTS: &[SlotDeclaration] = &[SlotDeclaration::userland()];
        const LEN: usize = manifest_len(SLOTS, "Namespace", "NamespaceAdmin");
        let bytes: [u8; LEN] = encode_manifest(SLOTS, "Namespace", "NamespaceAdmin", false);

        let manifest = decode_manifest(&bytes).expect("record decodes");
        assert_eq!(manifest.export(), Some("Namespace"));
        assert_eq!(manifest.control(), Some("NamespaceAdmin"));
        assert_eq!(manifest.slots().count(), 1);
    }

    #[test]
    fn a_record_without_a_control_protocol_names_none() {
        let bytes = device_record();

        let manifest = decode_manifest(&bytes).expect("record decodes");
        assert_eq!(manifest.control(), None);
    }

    #[test]
    fn a_flag_outside_the_declared_set_is_refused() {
        let mut bytes = device_record();
        bytes[OFFSET_FLAGS] = 0b10;

        assert_eq!(
            decode_manifest(&bytes),
            Err(ManifestDecodeError::InvalidLayout("flags"))
        );
    }

    #[test]
    fn a_directory_slot_carries_its_provider() {
        const SLOTS: &[SlotDeclaration] = &[SlotDeclaration::directory(IMAGE_FS)];
        const LEN: usize = manifest_len(SLOTS, "", "");
        let bytes: [u8; LEN] = encode_manifest(SLOTS, "", "", false);

        let manifest = decode_manifest(&bytes).expect("record decodes");
        let slots: Vec<ManifestSlot<'_>> = manifest.slots().collect();
        assert_eq!(slots[0].kind(), SlotKind::Directory);
        assert_eq!(names(&slots[0]), IMAGE_FS);
    }

    #[test]
    fn a_directory_slot_without_a_provider_is_refused() {
        const SLOTS: &[SlotDeclaration] = &[SlotDeclaration::directory(IMAGE_FS)];
        const LEN: usize = manifest_len(SLOTS, "", "");
        let mut bytes: [u8; LEN] = encode_manifest(SLOTS, "", "", false);
        bytes[COMPONENT_MANIFEST_HEADER_SIZE + 3] = 0;

        assert_eq!(
            decode_manifest(&bytes),
            Err(ManifestDecodeError::InvalidLayout("directory slot"))
        );
    }

    #[test]
    fn a_service_slot_carries_its_protocol() {
        const SLOTS: &[SlotDeclaration] = &[SlotDeclaration::service(NAMESPACE)];
        const LEN: usize = manifest_len(SLOTS, "", "");
        let bytes: [u8; LEN] = encode_manifest(SLOTS, "", "", false);

        let manifest = decode_manifest(&bytes).expect("record decodes");
        let slots: Vec<ManifestSlot<'_>> = manifest.slots().collect();
        assert_eq!(slots[0].kind(), SlotKind::Service);
        assert_eq!(names(&slots[0]), ["Namespace"]);
    }

    #[test]
    fn a_service_slot_carries_the_named_instance_past_its_protocol() {
        const SLOTS: &[SlotDeclaration] = &[SlotDeclaration::service(NAMED_FILE_SYSTEM)];
        const LEN: usize = manifest_len(SLOTS, "", "");
        let bytes: [u8; LEN] = encode_manifest(SLOTS, "", "", false);

        let manifest = decode_manifest(&bytes).expect("record decodes");
        let slots: Vec<ManifestSlot<'_>> = manifest.slots().collect();
        assert_eq!(slots[0].kind(), SlotKind::Service);
        assert_eq!(names(&slots[0]), ["FileSystem", "ramfs"]);
    }

    #[test]
    fn a_service_slot_without_names_is_refused() {
        const SLOTS: &[SlotDeclaration] = &[SlotDeclaration::service(NAMESPACE)];
        const LEN: usize = manifest_len(SLOTS, "", "");
        let mut bytes: [u8; LEN] = encode_manifest(SLOTS, "", "", false);
        bytes[COMPONENT_MANIFEST_HEADER_SIZE + 3] = 0;

        assert_eq!(
            decode_manifest(&bytes),
            Err(ManifestDecodeError::InvalidLayout("service slot"))
        );
    }

    #[test]
    fn a_control_slot_carries_the_named_instance_past_its_protocol() {
        const SLOTS: &[SlotDeclaration] = &[SlotDeclaration::control(NAMED_ADMIN)];
        const LEN: usize = manifest_len(SLOTS, "", "");
        let bytes: [u8; LEN] = encode_manifest(SLOTS, "", "", false);

        let manifest = decode_manifest(&bytes).expect("record decodes");
        let slots: Vec<ManifestSlot<'_>> = manifest.slots().collect();
        assert_eq!(slots[0].kind(), SlotKind::Control);
        assert_eq!(names(&slots[0]), ["NamespaceAdmin", "namespace-router"]);
    }

    #[test]
    fn a_control_slot_past_a_protocol_and_an_instance_is_refused() {
        const SLOTS: &[SlotDeclaration] = &[SlotDeclaration::control(NAMED_ADMIN)];
        const LEN: usize = manifest_len(SLOTS, "", "");
        let mut bytes: [u8; LEN] = encode_manifest(SLOTS, "", "", false);
        bytes[COMPONENT_MANIFEST_HEADER_SIZE + 3] = 3;

        assert_eq!(
            decode_manifest(&bytes),
            Err(ManifestDecodeError::InvalidLayout("control slot"))
        );
    }

    #[test]
    fn a_record_without_slots_decodes() {
        const LEN: usize = manifest_len(&[], "", "");
        let bytes: [u8; LEN] = encode_manifest(&[], "", "", false);

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
    fn a_service_slot_past_a_protocol_and_an_instance_is_refused() {
        const SLOTS: &[SlotDeclaration] = &[SlotDeclaration::service(NAMED_FILE_SYSTEM)];
        const LEN: usize = manifest_len(SLOTS, "", "");
        let mut bytes: [u8; LEN] = encode_manifest(SLOTS, "", "", false);
        bytes[COMPONENT_MANIFEST_HEADER_SIZE + 3] = 3;

        assert_eq!(
            decode_manifest(&bytes),
            Err(ManifestDecodeError::InvalidLayout("service slot"))
        );
    }

    #[test]
    fn a_userland_slot_with_a_resource_is_refused() {
        const SLOTS: &[SlotDeclaration] = &[SlotDeclaration::userland()];
        const LEN: usize = manifest_len(SLOTS, "", "");
        let mut bytes: [u8; LEN] = encode_manifest(SLOTS, "", "", false);
        bytes[COMPONENT_MANIFEST_HEADER_SIZE + 1] = 1;

        assert_eq!(
            decode_manifest(&bytes),
            Err(ManifestDecodeError::InvalidLayout("userland slot"))
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
