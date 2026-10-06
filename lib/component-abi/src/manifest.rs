//! Запись манифеста компонента: раскладка байт секции, её сборка и разбор.
//!
//! [`manifest_len`] считает длину записи, [`encode_manifest`] собирает её в const-контексте
//! под `static` программы, [`decode_manifest`] разбирает байты секции в
//! [`ComponentManifest`] с требованиями, протоколами экспортов, их фильтрами и признаками.
//!
//! Запись экспорта содержит длину протокола `u8`, байты протокола, число фильтров `u8` и фильтры
//! в кодировке [`Attributes`]. Записи экспортов предшествуют требованиям в порядке объявления.
//! Признаки - строки с длиной `u8` - следуют за требованиями.

use crate::{
    attributes::{Attribute, Attributes, write_attributes},
    startup::{LOG_REQUIREMENT, PROTOCOL_ID_MAX, REQUIREMENT_NAME_MAX},
};

/// Имя ELF-секции, в которой программа компонента содержит свой манифест.
pub const COMPONENT_MANIFEST_SECTION: &str = ".component.manifest";

const COMPONENT_MANIFEST_MAGIC: [u8; 8] = *b"CMPMFST\0";
const COMPONENT_MANIFEST_VERSION: u16 = 1;
const COMPONENT_MANIFEST_HEADER_SIZE: usize = 16;
const COMPONENT_REQUIREMENT_HEADER_SIZE: usize = 1;
const OFFSET_VERSION: usize = 8;
const OFFSET_REQUIREMENT_COUNT: usize = 10;
const OFFSET_EXPORT_COUNT: usize = 12;
const OFFSET_FEATURE_COUNT: usize = 13;
const OFFSET_RESERVED: usize = 14;

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

/// Экспорт из объявления манифеста: протокол и альтернативные фильтры.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportDeclaration {
    protocol: &'static str,
    filters: &'static [&'static [Attribute<'static>]],
}

/// Экспорт, прочитанный [`decode_manifest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManifestExport<'a> {
    protocol: &'a str,
    filters: &'a [u8],
    filter_count: u8,
}

/// Экспорты манифеста в порядке объявления.
#[derive(Debug, Clone)]
pub struct ManifestExports<'a> {
    bytes: &'a [u8],
    cursor: usize,
    remaining: u8,
}

/// Фильтры экспорта в порядке объявления.
#[derive(Debug, Clone)]
pub struct ManifestFilters<'a> {
    bytes: &'a [u8],
    remaining: u8,
}

/// Манифест компонента, разобранный из секции его программы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComponentManifest<'a> {
    requirements: &'a [u8],
    requirement_count: u16,
    exports: &'a [u8],
    export_count: u8,
    features: &'a [u8],
    feature_count: u8,
}

/// Признаки машины, которые требует манифест, в порядке объявления.
#[derive(Debug, Clone)]
pub struct ManifestFeatures<'a> {
    bytes: &'a [u8],
    cursor: usize,
    remaining: u8,
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

impl ExportDeclaration {
    /// Экспорт без фильтров. Он совпадает только с запросом без атрибутов.
    #[must_use]
    pub const fn new(protocol: &'static str) -> Self {
        Self {
            protocol,
            filters: &[],
        }
    }

    /// Экспорт с фильтрами `filters`. Каждый фильтр содержит хотя бы одну пару.
    #[must_use]
    pub const fn filtered(
        protocol: &'static str,
        filters: &'static [&'static [Attribute<'static>]],
    ) -> Self {
        Self { protocol, filters }
    }
}

impl<'a> ManifestExport<'a> {
    /// Идентификатор протокола экспорта.
    #[must_use]
    pub const fn protocol(&self) -> &'a str {
        self.protocol
    }

    #[must_use]
    pub const fn filters(&self) -> ManifestFilters<'a> {
        ManifestFilters {
            bytes: self.filters,
            remaining: self.filter_count,
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

    /// Экспорты компонента в порядке объявления.
    #[must_use]
    pub const fn exports(&self) -> ManifestExports<'a> {
        ManifestExports {
            bytes: self.exports,
            cursor: 0,
            remaining: self.export_count,
        }
    }

    /// Признаки машины, без публикации которых экспорты компонента скрыты от поиска.
    #[must_use]
    pub const fn features(&self) -> ManifestFeatures<'a> {
        ManifestFeatures {
            bytes: self.features,
            cursor: 0,
            remaining: self.feature_count,
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

impl<'a> Iterator for ManifestExports<'a> {
    type Item = ManifestExport<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }

        let (export, end) = parse_export(self.bytes, self.cursor).ok()?;
        self.remaining -= 1;
        self.cursor = end;
        Some(export)
    }
}

impl<'a> Iterator for ManifestFeatures<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }

        let (feature, end) = parse_name(self.bytes, self.cursor, "feature").ok()?;
        self.remaining -= 1;
        self.cursor = end;
        Some(feature)
    }
}

impl<'a> Iterator for ManifestFilters<'a> {
    type Item = Attributes<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }

        let (filter, rest) = Attributes::decode_prefix(self.bytes)?;
        self.remaining -= 1;
        self.bytes = rest;
        Some(filter)
    }
}

/// Длина записи манифеста.
#[must_use]
pub const fn manifest_len(
    requirements: &[RequirementDeclaration],
    exports: &[ExportDeclaration],
    features: &[&str],
) -> usize {
    let mut counter = Writer::counter();
    write_manifest(&mut counter, requirements, exports, features);
    counter.len
}

/// Кодирует полную запись манифеста. `LEN` равна [`manifest_len`].
#[must_use]
pub const fn encode_manifest<const LEN: usize>(
    requirements: &[RequirementDeclaration],
    exports: &[ExportDeclaration],
    features: &[&str],
) -> [u8; LEN] {
    let mut bytes = [0; LEN];
    encode_manifest_into(&mut bytes, requirements, exports, features);
    bytes
}

/// Кодирует манифест в буфер длиной [`manifest_len`].
pub const fn encode_manifest_into(
    bytes: &mut [u8],
    requirements: &[RequirementDeclaration],
    exports: &[ExportDeclaration],
    features: &[&str],
) {
    assert!(
        bytes.len() == manifest_len(requirements, exports, features),
        "buffer has the manifest length"
    );
    write_manifest(&mut Writer::new(bytes), requirements, exports, features);
}

pub(crate) struct Writer<'a> {
    bytes: Option<&'a mut [u8]>,
    pub(crate) len: usize,
}

impl<'a> Writer<'a> {
    pub(crate) const fn counter() -> Self {
        Self {
            bytes: None,
            len: 0,
        }
    }

    pub(crate) const fn new(bytes: &'a mut [u8]) -> Self {
        Self {
            bytes: Some(bytes),
            len: 0,
        }
    }

    pub(crate) const fn put(&mut self, data: &[u8]) {
        if let Some(bytes) = self.bytes.take() {
            // Длину буфера проверяет вызывающий по результату счётчика.
            let (written, rest) = bytes.split_at_mut(data.len());
            written.copy_from_slice(data);
            self.bytes = Some(rest);
        }

        self.len += data.len();
    }

    pub(crate) const fn put_len(&mut self, len: usize) {
        assert!(len <= u8::MAX as usize, "field length fits a byte");
        #[allow(clippy::cast_possible_truncation)]
        self.put(&[len as u8]);
    }

    const fn put_name(&mut self, name: &str) {
        assert!(!name.is_empty(), "name is non-empty");
        self.put_len(name.len());
        self.put(name.as_bytes());
    }
}

const fn write_manifest(
    out: &mut Writer<'_>,
    requirements: &[RequirementDeclaration],
    exports: &[ExportDeclaration],
    features: &[&str],
) {
    assert!(
        requirements.len() <= u16::MAX as usize,
        "requirement count fits the record"
    );
    assert!(
        exports.len() <= u8::MAX as usize,
        "export count fits the record"
    );
    assert!(
        features.len() <= u8::MAX as usize,
        "feature count fits the record"
    );
    #[allow(clippy::cast_possible_truncation)]
    let requirement_count = requirements.len() as u16;

    out.put(&COMPONENT_MANIFEST_MAGIC);
    out.put(&COMPONENT_MANIFEST_VERSION.to_le_bytes());
    out.put(&requirement_count.to_le_bytes());
    out.put_len(exports.len());
    out.put_len(features.len());
    out.put(&[0; COMPONENT_MANIFEST_HEADER_SIZE - OFFSET_RESERVED]);

    let mut declared = exports;
    while let [export, tail @ ..] = declared {
        assert!(
            export.protocol.len() <= PROTOCOL_ID_MAX,
            "export protocol fits PROTOCOL_ID_MAX"
        );
        out.put_name(export.protocol);
        out.put_len(export.filters.len());

        let mut filters = export.filters;
        while let [filter, others @ ..] = filters {
            assert!(!filter.is_empty(), "a declared filter holds a pair");
            write_attributes(out, filter);
            filters = others;
        }
        declared = tail;
    }

    let mut pending = requirements;
    while let [declaration, tail @ ..] = pending {
        assert!(
            declaration.name.len() <= REQUIREMENT_NAME_MAX,
            "requirement name fits REQUIREMENT_NAME_MAX"
        );
        assert!(
            declaration.protocol.len() <= PROTOCOL_ID_MAX,
            "requirement protocol fits PROTOCOL_ID_MAX"
        );
        out.put(&[declaration.mode as u8]);
        out.put_name(declaration.name);
        out.put_name(declaration.protocol);
        pending = tail;
    }

    let mut required = features;
    while let [feature, tail @ ..] = required {
        out.put_name(feature);
        required = tail;
    }
}

/// Разбирает запись манифеста. Буфер разбирается целиком. Хвост сверх записи отвергается.
/// Требование с именем [`LOG_REQUIREMENT`], повтор имени требования, повтор экспорта и повтор
/// признака отвергаются.
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
    let mut exports_end = COMPONENT_MANIFEST_HEADER_SIZE;
    for _ in 0..export_count {
        exports_end = parse_export(bytes, exports_end)?.1;
    }

    let mut requirements_end = exports_end;
    for _ in 0..requirement_count {
        requirements_end = parse_requirement(bytes, requirements_end)?.end;
    }

    let feature_count = *bytes
        .get(OFFSET_FEATURE_COUNT)
        .ok_or(ManifestDecodeError::InvalidLayout("feature count"))?;
    let mut cursor = requirements_end;
    for _ in 0..feature_count {
        cursor = parse_name(bytes, cursor, "feature")?.1;
    }
    if cursor != bytes.len() {
        return Err(ManifestDecodeError::Truncated {
            needed: cursor,
            actual: bytes.len(),
        });
    }

    let manifest = ComponentManifest {
        requirements: bytes
            .get(exports_end..requirements_end)
            .ok_or(ManifestDecodeError::InvalidLayout("requirements"))?,
        requirement_count,
        exports: bytes
            .get(COMPONENT_MANIFEST_HEADER_SIZE..exports_end)
            .ok_or(ManifestDecodeError::InvalidLayout("exports"))?,
        export_count,
        features: bytes
            .get(requirements_end..cursor)
            .ok_or(ManifestDecodeError::InvalidLayout("features"))?,
        feature_count,
    };
    // Экспорты отличаются только протоколом.
    for (index, export) in manifest.exports().enumerate() {
        if manifest
            .exports()
            .take(index)
            .any(|earlier| earlier.protocol() == export.protocol())
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

    for (index, feature) in manifest.features().enumerate() {
        if manifest
            .features()
            .take(index)
            .any(|earlier| earlier == feature)
        {
            return Err(ManifestDecodeError::InvalidLayout("feature"));
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

fn parse_export(
    bytes: &[u8],
    at: usize,
) -> Result<(ManifestExport<'_>, usize), ManifestDecodeError> {
    let (protocol, cursor) = parse_name(bytes, at, "export")?;
    let filter_count = *bytes.get(cursor).ok_or(ManifestDecodeError::Truncated {
        needed: cursor + 1,
        actual: bytes.len(),
    })?;

    let start = cursor + 1;
    let mut rest = bytes.get(start..).unwrap_or_default();
    for _ in 0..filter_count {
        let (filter, tail) =
            Attributes::decode_prefix(rest).ok_or(ManifestDecodeError::InvalidLayout("filter"))?;
        if filter.is_empty() {
            return Err(ManifestDecodeError::InvalidLayout("filter"));
        }
        rest = tail;
    }

    let end = bytes.len() - rest.len();
    let export = ManifestExport {
        protocol,
        filters: bytes
            .get(start..end)
            .ok_or(ManifestDecodeError::InvalidLayout("filter"))?,
        filter_count,
    };
    Ok((export, end))
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

    const NO_EXPORTS: &[ExportDeclaration] = &[];
    const TWO_EXPORTS: &[ExportDeclaration] = &[
        ExportDeclaration::new("Namespace"),
        ExportDeclaration::new("FileSystemMount"),
    ];
    const COMPATIBLE_A: &[Attribute<'static>] = &[Attribute {
        key: b"compatible",
        value: b"A",
    }];
    const COMPATIBLE_B_ROLE: &[Attribute<'static>] = &[
        Attribute {
            key: b"compatible",
            value: b"B",
        },
        Attribute {
            key: b"role",
            value: b"",
        },
    ];
    const FILTERED_EXPORTS: &[ExportDeclaration] = &[
        ExportDeclaration::filtered("Display", &[COMPATIBLE_A, COMPATIBLE_B_ROLE]),
        ExportDeclaration::new("Namespace"),
    ];
    const FILTERED_LEN: usize = manifest_len(&[], FILTERED_EXPORTS, &[]);

    fn protocols<'a>(manifest: &ComponentManifest<'a>) -> Vec<&'a str> {
        manifest.exports().map(|export| export.protocol()).collect()
    }

    const SESSION_REQUIREMENTS: &[RequirementDeclaration] =
        &[RequirementDeclaration::session("files", "FileSystem")];
    const SESSION_LEN: usize = manifest_len(SESSION_REQUIREMENTS, NO_EXPORTS, &[]);

    fn session_record() -> [u8; SESSION_LEN] {
        encode_manifest(SESSION_REQUIREMENTS, NO_EXPORTS, &[])
    }

    #[test]
    fn a_record_carries_every_requirement_export_and_feature_in_the_order_of_the_declaration() {
        const REQUIREMENTS: &[RequirementDeclaration] = &[
            RequirementDeclaration::session("image", "Image"),
            RequirementDeclaration::session("files", "FileSystem"),
            RequirementDeclaration::all("displays", "Display"),
        ];
        const FEATURES: &[&str] = &["virtio,mmio", "arm,pl031"];
        const LEN: usize = manifest_len(REQUIREMENTS, TWO_EXPORTS, FEATURES);
        let bytes: [u8; LEN] = encode_manifest(REQUIREMENTS, TWO_EXPORTS, FEATURES);

        let manifest = decode_manifest(&bytes).expect("record decodes");
        assert_eq!(protocols(&manifest), ["Namespace", "FileSystemMount"]);
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
        assert!(manifest.features().eq(FEATURES.iter().copied()));
    }

    #[test]
    fn filters_of_an_export_decode_in_the_order_of_the_declaration() {
        let bytes: [u8; FILTERED_LEN] = encode_manifest(&[], FILTERED_EXPORTS, &[]);

        let manifest = decode_manifest(&bytes).expect("record decodes");

        assert_eq!(protocols(&manifest), ["Display", "Namespace"]);

        let filters: Vec<Vec<Vec<Attribute<'_>>>> = manifest
            .exports()
            .map(|export| {
                export
                    .filters()
                    .map(|filter| filter.iter().collect())
                    .collect()
            })
            .collect();
        assert_eq!(
            filters,
            [
                std::vec![COMPATIBLE_A.to_vec(), COMPATIBLE_B_ROLE.to_vec()],
                std::vec![]
            ]
        );
    }

    #[test]
    fn an_export_is_its_protocol_and_its_filter_count() {
        const LEN: usize = manifest_len(&[], TWO_EXPORTS, &[]);
        let bytes: [u8; LEN] = encode_manifest(&[], TWO_EXPORTS, &[]);

        assert_eq!(
            &bytes[COMPONENT_MANIFEST_HEADER_SIZE..],
            b"\x09Namespace\x00\x0fFileSystemMount\x00"
        );
    }

    #[test]
    fn an_empty_filter_in_a_record_is_refused() {
        let mut bytes: [u8; FILTERED_LEN] = encode_manifest(&[], FILTERED_EXPORTS, &[]);
        let first_filter = COMPONENT_MANIFEST_HEADER_SIZE + 1 + "Display".len() + 1;
        bytes[first_filter] = 0;

        assert_eq!(
            decode_manifest(&bytes),
            Err(ManifestDecodeError::InvalidLayout("filter"))
        );
    }

    #[test]
    fn a_repeated_key_in_a_filter_is_refused() {
        const REPEATED: &[Attribute<'static>] = &[
            Attribute {
                key: b"compatible",
                value: b"A",
            },
            Attribute {
                key: b"compatible",
                value: b"B",
            },
        ];
        const EXPORTS: &[ExportDeclaration] =
            &[ExportDeclaration::filtered("Display", &[REPEATED])];
        const LEN: usize = manifest_len(&[], EXPORTS, &[]);
        let bytes: [u8; LEN] = encode_manifest(&[], EXPORTS, &[]);

        assert_eq!(
            decode_manifest(&bytes),
            Err(ManifestDecodeError::InvalidLayout("filter"))
        );
    }

    #[test]
    #[should_panic(expected = "a declared filter holds a pair")]
    fn encoding_an_empty_filter_panics() {
        let _ = manifest_len(&[], &[ExportDeclaration::filtered("Display", &[&[]])], &[]);
    }

    #[test]
    fn a_truncated_filter_is_refused() {
        let bytes: [u8; FILTERED_LEN] = encode_manifest(&[], FILTERED_EXPORTS, &[]);

        for length in COMPONENT_MANIFEST_HEADER_SIZE..bytes.len() {
            assert!(
                decode_manifest(&bytes[..length]).is_err(),
                "{length} bytes must not decode"
            );
        }
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
        const LEN: usize = manifest_len(&[], TWO_EXPORTS, &[]);
        let mut bytes: [u8; LEN] = encode_manifest(&[], TWO_EXPORTS, &[]);
        bytes[OFFSET_EXPORT_COUNT] = 9;

        assert!(matches!(
            decode_manifest(&bytes),
            Err(ManifestDecodeError::Truncated { .. })
        ));
    }

    #[test]
    fn an_export_repeated_in_a_record_is_refused() {
        const EXPORTS: &[ExportDeclaration] = &[
            ExportDeclaration::new("Namespace"),
            ExportDeclaration::filtered("Namespace", &[COMPATIBLE_A]),
        ];
        const LEN: usize = manifest_len(&[], EXPORTS, &[]);
        let bytes: [u8; LEN] = encode_manifest(&[], EXPORTS, &[]);

        assert_eq!(
            decode_manifest(&bytes),
            Err(ManifestDecodeError::InvalidLayout("export"))
        );
    }

    #[test]
    fn a_feature_repeated_in_a_record_is_refused() {
        const FEATURES: &[&str] = &["arm,pl031", "arm,pl031"];
        const LEN: usize = manifest_len(&[], NO_EXPORTS, FEATURES);
        let bytes: [u8; LEN] = encode_manifest(&[], NO_EXPORTS, FEATURES);

        assert_eq!(
            decode_manifest(&bytes),
            Err(ManifestDecodeError::InvalidLayout("feature"))
        );
    }

    #[test]
    fn an_empty_export_name_is_refused() {
        const LEN: usize = manifest_len(&[], TWO_EXPORTS, &[]);
        let mut bytes: [u8; LEN] = encode_manifest(&[], TWO_EXPORTS, &[]);
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
        const LEN: usize = manifest_len(REQUIREMENTS, NO_EXPORTS, &[]);
        let bytes: [u8; LEN] = encode_manifest(REQUIREMENTS, NO_EXPORTS, &[]);

        assert_eq!(
            decode_manifest(&bytes),
            Err(ManifestDecodeError::InvalidLayout("requirement name"))
        );
    }

    #[test]
    fn a_manifest_declaring_the_log_requirement_is_refused() {
        const REQUIREMENTS: &[RequirementDeclaration] =
            &[RequirementDeclaration::session(LOG_REQUIREMENT, "Log")];
        const LEN: usize = manifest_len(REQUIREMENTS, NO_EXPORTS, &[]);
        let bytes: [u8; LEN] = encode_manifest(REQUIREMENTS, NO_EXPORTS, &[]);

        assert_eq!(
            decode_manifest(&bytes),
            Err(ManifestDecodeError::InvalidLayout("log requirement"))
        );
    }

    #[test]
    #[should_panic(expected = "export count fits the record")]
    fn encoding_more_exports_than_a_byte_counts_panics() {
        let _ = manifest_len(&[], &[ExportDeclaration::new("A"); 256], &[]);
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
        const LEN: usize = manifest_len(&[], NO_EXPORTS, &[]);
        let bytes: [u8; LEN] = encode_manifest(&[], NO_EXPORTS, &[]);

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
