//! Запись манифеста компонента: раскладка байт секции, её сборка и разбор.
//!
//! [`manifest_len`] считает длину записи, [`encode_manifest`] собирает её в const-контексте
//! под `static` программы, [`decode_manifest`] разбирает байты секции в
//! [`ComponentManifest`] с требованиями и протоколами экспортов.

use crate::startup::{LOG_REQUIREMENT, PROTOCOL_ID_MAX, REQUIREMENT_NAME_MAX};

/// Имя ELF-секции, в которой программа компонента содержит свой манифест.
pub const COMPONENT_MANIFEST_SECTION: &str = ".component.manifest";

const COMPONENT_MANIFEST_MAGIC: [u8; 8] = *b"CMPMFST\0";
const COMPONENT_MANIFEST_VERSION: u16 = 1;
const COMPONENT_MANIFEST_HEADER_SIZE: usize = 16;
const COMPONENT_REQUIREMENT_HEADER_SIZE: usize = 1;
const OFFSET_VERSION: usize = 8;
const OFFSET_REQUIREMENT_COUNT: usize = 10;
const OFFSET_EXPORT_COUNT: usize = 12;
const OFFSET_RESERVED: usize = 13;

/// Кратность сессий протокола требования.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequirementMode {
    /// Сессия экспорта протокола, если провайдер доступен.
    One = 0,
    /// Все зарегистрированные экспортёры протокола.
    All = 1,
}

/// Требование из объявления манифеста, вход [`manifest_len`] и [`encode_manifest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequirementDeclaration {
    name: &'static str,
    mode: RequirementMode,
    protocol: &'static str,
}

/// Требование, прочитанное [`decode_manifest`] из байт секции [`COMPONENT_MANIFEST_SECTION`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManifestRequirement<'a> {
    name: &'a str,
    mode: RequirementMode,
    protocol: &'a str,
}

/// Имена записи в порядке объявления: длина `u8` и байты каждого.
#[derive(Debug, Clone)]
pub struct ManifestNames<'a> {
    bytes: &'a [u8],
    cursor: usize,
    remaining: u8,
}

/// Манифест компонента, разобранный из секции его программы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComponentManifest<'a> {
    requirements: &'a [u8],
    requirement_count: u16,
    exports: &'a [u8],
    export_count: u8,
}

/// Требования манифеста в порядке объявления.
#[derive(Debug, Clone)]
pub struct ManifestRequirements<'a> {
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
    /// Длина буфера расходится с длиной записи. `needed` называет конец записи, `actual` -
    /// длину буфера, и хвост сверх записи отвергается этой же ошибкой.
    Truncated { needed: usize, actual: usize },
    /// Поле записи вне множества своих значений.
    InvalidLayout(&'static str),
}

impl RequirementMode {
    /// Вид по его байту в записи. `None` для байта вне видов.
    #[must_use]
    pub const fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::One),
            1 => Some(Self::All),
            _ => None,
        }
    }
}

impl RequirementDeclaration {
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    #[must_use]
    pub const fn mode(&self) -> RequirementMode {
        self.mode
    }

    #[must_use]
    pub const fn protocol(&self) -> &'static str {
        self.protocol
    }

    #[must_use]
    pub const fn all(name: &'static str, protocol: &'static str) -> Self {
        Self {
            name,
            mode: RequirementMode::All,
            protocol,
        }
    }

    #[must_use]
    pub const fn session(name: &'static str, protocol: &'static str) -> Self {
        Self {
            name,
            mode: RequirementMode::One,
            protocol,
        }
    }
}

impl<'a> ManifestRequirement<'a> {
    /// Имя, под которым компонент запрашивает требование.
    #[must_use]
    pub const fn name(&self) -> &'a str {
        self.name
    }

    #[must_use]
    pub const fn mode(&self) -> RequirementMode {
        self.mode
    }

    /// Идентификатор протокола сессии.
    #[must_use]
    pub const fn protocol(&self) -> &'a str {
        self.protocol
    }
}

impl<'a> ComponentManifest<'a> {
    /// Требования в порядке объявления.
    #[must_use]
    pub const fn requirements(&self) -> ManifestRequirements<'a> {
        ManifestRequirements {
            bytes: self.requirements,
            cursor: 0,
            remaining: self.requirement_count,
        }
    }

    /// Идентификаторы протоколов экспортов компонента в порядке объявления.
    #[must_use]
    pub const fn exports(&self) -> ManifestNames<'a> {
        ManifestNames {
            bytes: self.exports,
            cursor: 0,
            remaining: self.export_count,
        }
    }
}

impl<'a> Iterator for ManifestRequirements<'a> {
    type Item = ManifestRequirement<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let parsed = parse_requirement(self.bytes, self.cursor).ok()?;
        self.remaining -= 1;
        self.cursor = parsed.end;
        Some(parsed.requirement)
    }
}

impl<'a> Iterator for ManifestNames<'a> {
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

/// Длина записи манифеста с заголовком, протоколами экспортов и требованиями.
#[must_use]
pub const fn manifest_len(requirements: &[RequirementDeclaration], exports: &[&str]) -> usize {
    let mut len = COMPONENT_MANIFEST_HEADER_SIZE;
    let mut protocols = exports;
    while let [export, tail @ ..] = protocols {
        len += 1 + export.len();
        protocols = tail;
    }
    let mut pending = requirements;
    while let [requirement, tail @ ..] = pending {
        len += COMPONENT_REQUIREMENT_HEADER_SIZE
            + 1
            + requirement.name.len()
            + 1
            + requirement.protocol.len();
        pending = tail;
    }
    len
}

/// Кодирует запись манифеста целиком. `LEN` равна [`manifest_len`]. `exports` называет
/// идентификаторы протоколов экспортов в порядке объявления.
#[must_use]
pub const fn encode_manifest<const LEN: usize>(
    requirements: &[RequirementDeclaration],
    exports: &[&str],
) -> [u8; LEN] {
    let mut bytes = [0; LEN];
    encode_manifest_into(&mut bytes, requirements, exports);
    bytes
}

/// Кодирует манифест в буфер длиной [`manifest_len`]. Те же ограничения, что у [`encode_manifest`].
pub const fn encode_manifest_into(
    bytes: &mut [u8],
    requirements: &[RequirementDeclaration],
    exports: &[&str],
) {
    assert!(
        bytes.len() == manifest_len(requirements, exports),
        "record length covers the header, every export and every requirement"
    );

    let (header_bytes, mut rest) = bytes.split_at_mut(COMPONENT_MANIFEST_HEADER_SIZE);

    let mut export_count: u8 = 0;
    let mut protocols = exports;
    while let [export, tail @ ..] = protocols {
        assert!(export_count < u8::MAX, "export count fits the record");
        assert!(
            export.len() <= PROTOCOL_ID_MAX,
            "export protocol fits PROTOCOL_ID_MAX"
        );
        rest = write_name(rest, export);
        export_count += 1;
        protocols = tail;
    }

    let mut requirement_count: u16 = 0;
    let mut pending = requirements;
    while let [declaration, tail @ ..] = pending {
        assert!(
            requirement_count < u16::MAX,
            "requirement count fits the record"
        );
        assert!(
            declaration.name.len() <= REQUIREMENT_NAME_MAX,
            "requirement name fits REQUIREMENT_NAME_MAX"
        );
        assert!(
            declaration.protocol.len() <= PROTOCOL_ID_MAX,
            "requirement protocol fits PROTOCOL_ID_MAX"
        );
        rest = write_bytes(rest, &[declaration.mode as u8]);
        rest = write_name(rest, declaration.name);
        rest = write_name(rest, declaration.protocol);
        requirement_count += 1;
        pending = tail;
    }

    let mut header = [0; COMPONENT_MANIFEST_HEADER_SIZE];
    let (magic, _) = header.split_at_mut(COMPONENT_MANIFEST_MAGIC.len());
    magic.copy_from_slice(&COMPONENT_MANIFEST_MAGIC);
    let version = COMPONENT_MANIFEST_VERSION.to_le_bytes();
    header[OFFSET_VERSION] = version[0];
    header[OFFSET_VERSION + 1] = version[1];
    let count = requirement_count.to_le_bytes();
    header[OFFSET_REQUIREMENT_COUNT] = count[0];
    header[OFFSET_REQUIREMENT_COUNT + 1] = count[1];
    header[OFFSET_EXPORT_COUNT] = export_count;
    header_bytes.copy_from_slice(&header);
}

#[allow(clippy::cast_possible_truncation)]
const fn len_byte(len: usize) -> u8 {
    assert!(len <= u8::MAX as usize, "field length fits a byte");
    len as u8
}

const fn write_name<'a>(bytes: &'a mut [u8], name: &str) -> &'a mut [u8] {
    let text = name.as_bytes();
    assert!(!text.is_empty(), "name is non-empty");
    let rest = write_bytes(bytes, &[len_byte(text.len())]);
    write_bytes(rest, text)
}

const fn write_bytes<'a>(bytes: &'a mut [u8], text: &[u8]) -> &'a mut [u8] {
    // `LEN` равна `manifest_len`, и остаток буфера покрывает `text`.
    let (written, rest) = bytes.split_at_mut(text.len());
    written.copy_from_slice(text);
    rest
}

/// Разбирает запись манифеста. Буфер разбирается целиком. Хвост сверх записи отвергается.
/// Требование с именем [`LOG_REQUIREMENT`], повтор имени требования и повтор экспорта
/// отвергаются.
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

    let requirement_count = read_u16(bytes, OFFSET_REQUIREMENT_COUNT);
    let export_count = *bytes
        .get(OFFSET_EXPORT_COUNT)
        .ok_or(ManifestDecodeError::InvalidLayout("export count"))?;
    let exports_end = parse_names(
        bytes,
        COMPONENT_MANIFEST_HEADER_SIZE,
        export_count,
        "export",
    )?;

    let mut cursor = exports_end;
    for _ in 0..requirement_count {
        cursor = parse_requirement(bytes, cursor)?.end;
    }
    if cursor != bytes.len() {
        return Err(ManifestDecodeError::Truncated {
            needed: cursor,
            actual: bytes.len(),
        });
    }

    let manifest = ComponentManifest {
        requirements: bytes
            .get(exports_end..cursor)
            .ok_or(ManifestDecodeError::InvalidLayout("requirements"))?,
        requirement_count,
        exports: bytes
            .get(COMPONENT_MANIFEST_HEADER_SIZE..exports_end)
            .ok_or(ManifestDecodeError::InvalidLayout("exports"))?,
        export_count,
    };
    // Экспорты отличаются только протоколом.
    for (index, export) in manifest.exports().enumerate() {
        if manifest
            .exports()
            .take(index)
            .any(|earlier| earlier == export)
        {
            return Err(ManifestDecodeError::InvalidLayout("export"));
        }
    }
    // Компонент и план называют требование по имени, поэтому имя не повторяется. Имя `log`
    // занято журналом, который компонент получает без объявления.
    for (index, requirement) in manifest.requirements().enumerate() {
        if requirement.name() == LOG_REQUIREMENT {
            return Err(ManifestDecodeError::InvalidLayout("log requirement"));
        }
        if manifest
            .requirements()
            .take(index)
            .any(|earlier| earlier.name() == requirement.name())
        {
            return Err(ManifestDecodeError::InvalidLayout("requirement name"));
        }
    }
    Ok(manifest)
}

fn parse_name<'a>(
    bytes: &'a [u8],
    at: usize,
    field: &'static str,
) -> Result<(&'a str, usize), ManifestDecodeError> {
    let len = usize::from(*bytes.get(at).ok_or(ManifestDecodeError::Truncated {
        needed: at + 1,
        actual: bytes.len(),
    })?);
    if len == 0 {
        return Err(ManifestDecodeError::InvalidLayout(field));
    }
    let end = at + 1 + len;
    let name = bytes
        .get(at + 1..end)
        .ok_or(ManifestDecodeError::Truncated {
            needed: end,
            actual: bytes.len(),
        })?;
    let name = core::str::from_utf8(name).map_err(|_| ManifestDecodeError::InvalidLayout(field))?;
    Ok((name, end))
}

fn parse_names(
    bytes: &[u8],
    at: usize,
    count: u8,
    field: &'static str,
) -> Result<usize, ManifestDecodeError> {
    let mut cursor = at;
    for _ in 0..count {
        cursor = parse_name(bytes, cursor, field)?.1;
    }
    Ok(cursor)
}

struct ParsedRequirement<'a> {
    requirement: ManifestRequirement<'a>,
    end: usize,
}

fn parse_requirement(
    bytes: &[u8],
    at: usize,
) -> Result<ParsedRequirement<'_>, ManifestDecodeError> {
    let mode = *bytes.get(at).ok_or(ManifestDecodeError::Truncated {
        needed: at + COMPONENT_REQUIREMENT_HEADER_SIZE,
        actual: bytes.len(),
    })?;
    let mode = RequirementMode::from_byte(mode)
        .ok_or(ManifestDecodeError::InvalidLayout("requirement mode"))?;
    let (name, cursor) = parse_name(
        bytes,
        at + COMPONENT_REQUIREMENT_HEADER_SIZE,
        "requirement name",
    )?;
    let (protocol, end) = parse_name(bytes, cursor, "requirement protocol")?;

    Ok(ParsedRequirement {
        requirement: ManifestRequirement {
            name,
            mode,
            protocol,
        },
        end,
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

    const NO_EXPORTS: &[&str] = &[];
    const TWO_EXPORTS: &[&str] = &["Namespace", "FileSystemMount"];

    const SESSION_REQUIREMENTS: &[RequirementDeclaration] =
        &[RequirementDeclaration::session("files", "FileSystem")];
    const SESSION_LEN: usize = manifest_len(SESSION_REQUIREMENTS, NO_EXPORTS);

    fn session_record() -> [u8; SESSION_LEN] {
        encode_manifest(SESSION_REQUIREMENTS, NO_EXPORTS)
    }

    #[test]
    fn a_record_carries_every_requirement_and_export_in_the_order_of_the_declaration() {
        const REQUIREMENTS: &[RequirementDeclaration] = &[
            RequirementDeclaration::session("image", "Image"),
            RequirementDeclaration::session("files", "FileSystem"),
            RequirementDeclaration::all("displays", "Display"),
        ];
        const LEN: usize = manifest_len(REQUIREMENTS, TWO_EXPORTS);
        let bytes: [u8; LEN] = encode_manifest(REQUIREMENTS, TWO_EXPORTS);

        let manifest = decode_manifest(&bytes).expect("record decodes");
        assert_eq!(manifest.exports().collect::<Vec<_>>(), TWO_EXPORTS);
        let requirements: Vec<_> = manifest
            .requirements()
            .map(|requirement| {
                (
                    requirement.name(),
                    requirement.mode(),
                    requirement.protocol(),
                )
            })
            .collect();
        assert_eq!(
            requirements,
            [
                ("image", RequirementMode::One, "Image"),
                ("files", RequirementMode::One, "FileSystem"),
                ("displays", RequirementMode::All, "Display"),
            ]
        );
    }

    #[test]
    fn a_requirement_is_its_mode_byte_name_and_protocol() {
        let bytes = session_record();

        assert_eq!(
            &bytes[COMPONENT_MANIFEST_HEADER_SIZE..],
            b"\x00\x05files\x0aFileSystem"
        );
    }

    #[test]
    fn an_export_count_past_the_listed_names_is_refused() {
        const LEN: usize = manifest_len(&[], TWO_EXPORTS);
        let mut bytes: [u8; LEN] = encode_manifest(&[], TWO_EXPORTS);
        bytes[OFFSET_EXPORT_COUNT] = 9;

        assert!(matches!(
            decode_manifest(&bytes),
            Err(ManifestDecodeError::Truncated { .. })
        ));
    }

    #[test]
    fn an_export_repeated_in_a_record_is_refused() {
        const EXPORTS: &[&str] = &["Namespace", "Namespace"];
        const LEN: usize = manifest_len(&[], EXPORTS);
        let bytes: [u8; LEN] = encode_manifest(&[], EXPORTS);

        assert_eq!(
            decode_manifest(&bytes),
            Err(ManifestDecodeError::InvalidLayout("export"))
        );
    }

    #[test]
    fn an_empty_export_name_is_refused() {
        const LEN: usize = manifest_len(&[], TWO_EXPORTS);
        let mut bytes: [u8; LEN] = encode_manifest(&[], TWO_EXPORTS);
        bytes[COMPONENT_MANIFEST_HEADER_SIZE] = 0;

        assert_eq!(
            decode_manifest(&bytes),
            Err(ManifestDecodeError::InvalidLayout("export"))
        );
    }

    #[test]
    fn a_requirement_name_repeated_in_a_record_is_refused() {
        const REQUIREMENTS: &[RequirementDeclaration] = &[
            RequirementDeclaration::session("files", "FileSystem"),
            RequirementDeclaration::all("files", "FileSystem"),
        ];
        const LEN: usize = manifest_len(REQUIREMENTS, NO_EXPORTS);
        let bytes: [u8; LEN] = encode_manifest(REQUIREMENTS, NO_EXPORTS);

        assert_eq!(
            decode_manifest(&bytes),
            Err(ManifestDecodeError::InvalidLayout("requirement name"))
        );
    }

    #[test]
    fn a_manifest_declaring_the_log_requirement_is_refused() {
        const REQUIREMENTS: &[RequirementDeclaration] =
            &[RequirementDeclaration::session(LOG_REQUIREMENT, "Log")];
        const LEN: usize = manifest_len(REQUIREMENTS, NO_EXPORTS);
        let bytes: [u8; LEN] = encode_manifest(REQUIREMENTS, NO_EXPORTS);

        assert_eq!(
            decode_manifest(&bytes),
            Err(ManifestDecodeError::InvalidLayout("log requirement"))
        );
    }

    #[test]
    #[should_panic(expected = "export count fits the record")]
    fn encoding_more_exports_than_a_byte_counts_panics() {
        const EXPORTS: &[&str] = &["A"; 256];
        const LEN: usize = manifest_len(&[], EXPORTS);
        let _: [u8; LEN] = encode_manifest(&[], EXPORTS);
    }

    #[test]
    fn an_empty_requirement_name_is_refused() {
        let mut bytes = session_record();
        bytes[COMPONENT_MANIFEST_HEADER_SIZE + COMPONENT_REQUIREMENT_HEADER_SIZE] = 0;

        assert_eq!(
            decode_manifest(&bytes),
            Err(ManifestDecodeError::InvalidLayout("requirement name"))
        );
    }

    #[test]
    fn an_empty_session_protocol_is_refused() {
        let mut bytes = session_record();
        let protocol_at = COMPONENT_MANIFEST_HEADER_SIZE + COMPONENT_REQUIREMENT_HEADER_SIZE + 6;
        bytes[protocol_at] = 0;

        assert_eq!(
            decode_manifest(&bytes),
            Err(ManifestDecodeError::InvalidLayout("requirement protocol"))
        );
    }

    #[test]
    fn a_record_without_requirements_decodes() {
        const LEN: usize = manifest_len(&[], NO_EXPORTS);
        let bytes: [u8; LEN] = encode_manifest(&[], NO_EXPORTS);

        let manifest = decode_manifest(&bytes).expect("record decodes");
        assert_eq!(manifest.requirements().count(), 0);
    }

    #[test]
    fn a_foreign_magic_is_refused() {
        let mut bytes = session_record();
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
        let mut bytes = session_record();
        for version in [2, 9] {
            bytes[OFFSET_VERSION] = version;
            assert_eq!(
                decode_manifest(&bytes),
                Err(ManifestDecodeError::InvalidVersion(u16::from(version)))
            );
        }
    }

    #[test]
    fn bytes_past_the_record_are_refused() {
        let mut bytes = session_record().to_vec();
        bytes.push(0);

        assert!(matches!(
            decode_manifest(&bytes),
            Err(ManifestDecodeError::Truncated { .. })
        ));
    }

    #[test]
    fn an_unknown_requirement_mode_is_refused() {
        let mut bytes = session_record();
        for mode in [2, 9] {
            bytes[COMPONENT_MANIFEST_HEADER_SIZE] = mode;
            assert_eq!(
                decode_manifest(&bytes),
                Err(ManifestDecodeError::InvalidLayout("requirement mode"))
            );
        }
    }

    #[test]
    fn a_non_zero_reserved_field_is_refused() {
        let mut bytes = session_record();
        bytes[OFFSET_RESERVED] = 1;

        assert_eq!(
            decode_manifest(&bytes),
            Err(ManifestDecodeError::InvalidLayout("reserved"))
        );
    }

    #[test]
    fn a_truncated_record_is_refused() {
        let bytes = session_record();
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
