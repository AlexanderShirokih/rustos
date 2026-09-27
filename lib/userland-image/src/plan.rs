use component_abi::{LOG_REQUIREMENT, SESSION_SCOPE_MAX};
use userland::entry_name;

/// Путь записи образа, из которой rootkeeper читает план загрузочной композиции.
pub const BOOT_PLAN_PATH: &str = "/rootkeeper/plan";

/// Имя записи с программой в каталоге компонента.
pub const COMPONENT_PROGRAM_NAME: &str = "program";

/// Имя записи `program` внутри каталога компонента - имя этого каталога. Любая другая
/// запись, в том числе другой файл того же каталога, названа собственным именем.
#[must_use]
pub fn component_name(record: &str) -> &str {
    match record.rsplit_once('/') {
        Some((directory, name))
            if name == COMPONENT_PROGRAM_NAME && !entry_name(directory).is_empty() =>
        {
            entry_name(directory)
        }
        _ => entry_name(record),
    }
}

pub const BOOT_PLAN_MAGIC: [u8; 8] = *b"RKPLAN\0\0";
pub const BOOT_PLAN_VERSION: u16 = 1;

pub(crate) const PLAN_HEADER_SIZE: usize = 16;
pub(crate) const COMPONENT_HEADER_SIZE: usize = 24;
pub(crate) const REQUIREMENT_HEADER_SIZE: usize = 16;

pub(crate) const COMPONENT_RESTARTS: usize = 0;
pub(crate) const COMPONENT_EXECUTABLE_LEN: usize = 4;
pub(crate) const COMPONENT_NAME_LEN: usize = 6;
pub(crate) const COMPONENT_REQUIREMENT_COUNT: usize = 8;
pub(crate) const COMPONENT_EXPORT_COUNT: usize = 10;
pub(crate) const COMPONENT_RESERVED: usize = 12;
pub(crate) const COMPONENT_MEMORY_LIMIT: usize = 16;

pub(crate) const REQUIREMENT_KIND: usize = 0;
pub(crate) const REQUIREMENT_RESERVED: usize = 1;
pub(crate) const REQUIREMENT_NAME_LEN: usize = 2;
pub(crate) const REQUIREMENT_PROVIDER: usize = 4;
pub(crate) const REQUIREMENT_PROTOCOL_LEN: usize = 6;
pub(crate) const REQUIREMENT_SCOPE_LEN: usize = 8;
pub(crate) const REQUIREMENT_INSTANCE: usize = 10;
pub(crate) const REQUIREMENT_RESOURCE: usize = 12;
pub(crate) const REQUIREMENT_SELECTOR_COUNT: usize = 14;

pub(crate) const REQUIREMENT_KIND_REGION: u8 = 0;
pub(crate) const REQUIREMENT_KIND_IRQ: u8 = 1;
pub(crate) const REQUIREMENT_KIND_MMIO: u8 = 2;
pub(crate) const REQUIREMENT_KIND_EXPORT: u8 = 3;

pub(crate) const NAME_LEN_SIZE: usize = 2;

/// Устройство требования `irq` или `mmio`: строки совместимости драйвера, номер экземпляра и
/// номер ресурса.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanDevice<'a> {
    selectors: &'a [u8],
    selector_count: u16,
    /// Номер экземпляра среди устройств той же совместимости.
    pub instance: u16,
    /// Номер ресурса устройства: линии либо окна.
    pub resource: u16,
}

/// Строки записи в порядке объявления: длина `u16` LE и байты каждой.
#[derive(Debug, Clone)]
pub struct PlanNames<'a> {
    bytes: &'a [u8],
    cursor: usize,
    remaining: u16,
}

impl<'a> PlanDevice<'a> {
    /// Строки совместимости, любая из которых подходит драйверу.
    #[must_use]
    pub const fn compatible(&self) -> PlanNames<'a> {
        PlanNames {
            bytes: self.selectors,
            cursor: 0,
            remaining: self.selector_count,
        }
    }
}

impl<'a> Iterator for PlanNames<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let len = usize::from(read_u16(self.bytes, self.cursor).ok()?);
        let at = self.cursor + NAME_LEN_SIZE;
        let name = core::str::from_utf8(self.bytes.get(at..at + len)?).ok()?;
        self.remaining -= 1;
        self.cursor = at + len;
        Some(name)
    }
}

/// Источник capability, которую родитель выдаёт на запрос требования.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequirementSource<'a> {
    /// Сессия экспорта `protocol` в области `scope`. `provider` - номер провайдера в плане.
    /// Пустая область называет весь экспорт.
    Export {
        provider: u16,
        protocol: &'a str,
        scope: &'a str,
    },
    /// Линия прерывания устройства.
    Irq(PlanDevice<'a>),
    /// MMIO-окно устройства.
    Mmio(PlanDevice<'a>),
    /// Read-only регион userland-образа.
    Region,
}

/// Требование компонента с именем из манифеста и его источник.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanRequirement<'a> {
    pub name: &'a str,
    pub source: RequirementSource<'a>,
}

/// Компонент загрузочной композиции.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanComponent<'a> {
    executable: &'a str,
    name: &'a str,
    requirements: &'a [u8],
    requirement_count: u16,
    exports: &'a [u8],
    export_count: u16,
    memory_limit: u64,
    restarts: u32,
}

/// План загрузочной композиции, разобранный из записи образа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootPlan<'a> {
    bytes: &'a [u8],
    count: u16,
}

/// Компоненты плана в порядке записи.
#[derive(Debug, Clone)]
pub struct PlanComponents<'a> {
    bytes: &'a [u8],
    cursor: usize,
    remaining: u16,
}

/// Требования компонента в порядке манифеста.
#[derive(Debug, Clone)]
pub struct PlanRequirements<'a> {
    bytes: &'a [u8],
    cursor: usize,
    remaining: u16,
}

/// Причина отказа декодера записи плана.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanDecodeError {
    /// Первые байты записи не равны [`BOOT_PLAN_MAGIC`].
    InvalidMagic([u8; 8]),
    /// Версия записи не равна [`BOOT_PLAN_VERSION`].
    InvalidVersion(u16),
    /// Буфера не хватает на поле по текущему смещению.
    Truncated { needed: usize, actual: usize },
    /// Поле нарушает форму записи. Имя поля называет причину.
    InvalidLayout(&'static str),
}

struct ParsedComponent<'a> {
    component: PlanComponent<'a>,
    end: usize,
}

struct ParsedRequirement<'a> {
    requirement: PlanRequirement<'a>,
    end: usize,
}

/// Декодирует запись плана. Буфер разбирается целиком, а хвост сверх плана
/// отвергается.
pub fn decode_plan(bytes: &[u8]) -> Result<BootPlan<'_>, PlanDecodeError> {
    BootPlan::decode(bytes)
}

impl<'a> PlanComponent<'a> {
    /// Каталог образа с программой компонента. Программа лежит в нём записью
    /// [`COMPONENT_PROGRAM_NAME`]. Один каталог композиция поднимает несколькими
    /// экземплярами.
    #[must_use]
    pub const fn executable(&self) -> &'a str {
        self.executable
    }

    /// Композиция называет компонент этим именем экземпляра. Под тем же именем идут его
    /// процесс и строки журнала.
    #[must_use]
    pub const fn name(&self) -> &'a str {
        self.name
    }

    /// Требования компонента в порядке манифеста.
    #[must_use]
    pub const fn requirements(&self) -> PlanRequirements<'a> {
        PlanRequirements {
            bytes: self.requirements,
            cursor: 0,
            remaining: self.requirement_count,
        }
    }

    /// Источник требования `name`.
    #[must_use]
    pub fn requirement(&self, name: &str) -> Option<RequirementSource<'a>> {
        self.requirements()
            .find(|requirement| requirement.name == name)
            .map(|requirement| requirement.source)
    }

    /// Протоколы экспортов компонента, по `Protocol::ID` на экспорт.
    #[must_use]
    pub const fn exports(&self) -> PlanNames<'a> {
        PlanNames {
            bytes: self.exports,
            cursor: 0,
            remaining: self.export_count,
        }
    }

    /// Предел памяти компонента в байтах. Родитель отрезает компоненту бюджет не меньше
    /// предела.
    #[must_use]
    pub const fn memory_limit(&self) -> u64 {
        self.memory_limit
    }

    /// Число перезапусков после первого запуска.
    #[must_use]
    pub const fn restarts(&self) -> u32 {
        self.restarts
    }
}

impl<'a> BootPlan<'a> {
    /// Число компонентов композиции.
    #[must_use]
    pub const fn len(&self) -> u16 {
        self.count
    }

    /// Декодер отвергает пустую композицию плана.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Компоненты композиции в порядке записи.
    #[must_use]
    pub const fn components(&self) -> PlanComponents<'a> {
        PlanComponents {
            bytes: self.bytes,
            cursor: PLAN_HEADER_SIZE,
            remaining: self.count,
        }
    }

    /// Компонент с номером `index`.
    #[must_use]
    pub fn component(&self, index: u16) -> Option<PlanComponent<'a>> {
        self.components().nth(usize::from(index))
    }

    fn decode(bytes: &'a [u8]) -> Result<Self, PlanDecodeError> {
        let header = bytes
            .get(..PLAN_HEADER_SIZE)
            .ok_or(PlanDecodeError::Truncated {
                needed: PLAN_HEADER_SIZE,
                actual: bytes.len(),
            })?;
        let magic = *header
            .first_chunk::<8>()
            .ok_or_else(|| truncated(bytes, 8))?;
        if magic != BOOT_PLAN_MAGIC {
            return Err(PlanDecodeError::InvalidMagic(magic));
        }
        let version = read_u16(header, 8)?;
        if version != BOOT_PLAN_VERSION {
            return Err(PlanDecodeError::InvalidVersion(version));
        }
        let count = read_u16(header, 10)?;
        if count == 0 {
            return Err(PlanDecodeError::InvalidLayout("component_count"));
        }
        if read_u32(header, 12)? != 0 {
            return Err(PlanDecodeError::InvalidLayout("reserved"));
        }

        let mut cursor = PLAN_HEADER_SIZE;
        for _ in 0..count {
            cursor = parse_component(bytes, cursor)?.end;
        }
        if cursor != bytes.len() {
            return Err(PlanDecodeError::InvalidLayout("trailing_bytes"));
        }

        let plan = Self { bytes, count };
        for (consumer, component) in plan.components().enumerate() {
            // Родитель ищет источник требования по имени, поэтому имя в записи не повторяется, а
            // имя `log` зарезервировано за журналом, который сам выдаёт родитель.
            for (index, requirement) in component.requirements().enumerate() {
                if requirement.name == LOG_REQUIREMENT
                    || component
                        .requirements()
                        .take(index)
                        .any(|earlier| earlier.name == requirement.name)
                {
                    return Err(PlanDecodeError::InvalidLayout("requirement_name"));
                }
                if let RequirementSource::Export {
                    provider, protocol, ..
                } = requirement.source
                {
                    plan.validate_export(consumer, provider, protocol)?;
                }
            }
        }
        Ok(plan)
    }

    fn validate_export(
        &self,
        consumer: usize,
        provider: u16,
        protocol: &str,
    ) -> Result<(), PlanDecodeError> {
        if usize::from(provider) == consumer {
            return Err(PlanDecodeError::InvalidLayout("requirement_provider"));
        }
        let exporter = self
            .component(provider)
            .ok_or(PlanDecodeError::InvalidLayout("requirement_provider"))?;
        if !exporter.exports().any(|export| export == protocol) {
            return Err(PlanDecodeError::InvalidLayout("requirement_protocol"));
        }
        Ok(())
    }
}

impl<'a> Iterator for PlanComponents<'a> {
    type Item = PlanComponent<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        // Запись уже проверена декодированием плана целиком, а повторный разбор
        // читает те же байты.
        let parsed = parse_component(self.bytes, self.cursor).ok()?;
        self.remaining -= 1;
        self.cursor = parsed.end;
        Some(parsed.component)
    }
}

impl<'a> Iterator for PlanRequirements<'a> {
    type Item = PlanRequirement<'a>;

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

fn parse_component(bytes: &[u8], at: usize) -> Result<ParsedComponent<'_>, PlanDecodeError> {
    let header = bytes
        .get(at..at + COMPONENT_HEADER_SIZE)
        .ok_or(truncated(bytes, at + COMPONENT_HEADER_SIZE))?;
    let restarts = read_u32(header, COMPONENT_RESTARTS)?;
    let executable_len = usize::from(read_u16(header, COMPONENT_EXECUTABLE_LEN)?);
    let name_len = usize::from(read_u16(header, COMPONENT_NAME_LEN)?);
    let requirement_count = read_u16(header, COMPONENT_REQUIREMENT_COUNT)?;
    let export_count = read_u16(header, COMPONENT_EXPORT_COUNT)?;
    if read_u32(header, COMPONENT_RESERVED)? != 0 {
        return Err(PlanDecodeError::InvalidLayout("reserved"));
    }
    let memory_limit = read_u64(header, COMPONENT_MEMORY_LIMIT)?;
    if memory_limit == 0 {
        return Err(PlanDecodeError::InvalidLayout("memory_limit"));
    }

    let cursor = at + COMPONENT_HEADER_SIZE;
    let (executable, cursor) = parse_str(bytes, cursor, executable_len, "executable")?;
    let (name, mut cursor) = parse_str(bytes, cursor, name_len, "name")?;

    let requirements_at = cursor;
    for _ in 0..requirement_count {
        cursor = parse_requirement(bytes, cursor)?.end;
    }
    let requirements = bytes
        .get(requirements_at..cursor)
        .ok_or(truncated(bytes, cursor))?;

    let exports_at = cursor;
    cursor = parse_names(bytes, cursor, export_count, "export")?;
    let exports = bytes
        .get(exports_at..cursor)
        .ok_or(truncated(bytes, cursor))?;

    let component = PlanComponent {
        executable,
        name,
        requirements,
        requirement_count,
        exports,
        export_count,
        memory_limit,
        restarts,
    };

    Ok(ParsedComponent {
        component,
        end: cursor,
    })
}

fn parse_requirement(bytes: &[u8], at: usize) -> Result<ParsedRequirement<'_>, PlanDecodeError> {
    let header = bytes
        .get(at..at + REQUIREMENT_HEADER_SIZE)
        .ok_or(truncated(bytes, at + REQUIREMENT_HEADER_SIZE))?;
    let kind = *header
        .get(REQUIREMENT_KIND)
        .ok_or(truncated(bytes, at + REQUIREMENT_KIND + 1))?;
    if !matches!(
        kind,
        REQUIREMENT_KIND_REGION
            | REQUIREMENT_KIND_IRQ
            | REQUIREMENT_KIND_MMIO
            | REQUIREMENT_KIND_EXPORT
    ) {
        return Err(PlanDecodeError::InvalidLayout("requirement_kind"));
    }
    if *header
        .get(REQUIREMENT_RESERVED)
        .ok_or(truncated(bytes, at + REQUIREMENT_RESERVED + 1))?
        != 0
    {
        return Err(PlanDecodeError::InvalidLayout("reserved"));
    }
    let name_len = usize::from(read_u16(header, REQUIREMENT_NAME_LEN)?);
    let provider = read_u16(header, REQUIREMENT_PROVIDER)?;
    let protocol_len = usize::from(read_u16(header, REQUIREMENT_PROTOCOL_LEN)?);
    let scope_len = usize::from(read_u16(header, REQUIREMENT_SCOPE_LEN)?);
    let instance = read_u16(header, REQUIREMENT_INSTANCE)?;
    let resource = read_u16(header, REQUIREMENT_RESOURCE)?;
    let selector_count = read_u16(header, REQUIREMENT_SELECTOR_COUNT)?;

    let export_kind = kind == REQUIREMENT_KIND_EXPORT;
    let device_kind = matches!(kind, REQUIREMENT_KIND_IRQ | REQUIREMENT_KIND_MMIO);
    // Провайдера, протокол и область называет только требование сессии.
    if !export_kind && provider != 0 {
        return Err(PlanDecodeError::InvalidLayout("requirement_provider"));
    }
    if !export_kind && protocol_len != 0 {
        return Err(PlanDecodeError::InvalidLayout("requirement_protocol"));
    }
    if (!export_kind && scope_len != 0) || scope_len > SESSION_SCOPE_MAX {
        return Err(PlanDecodeError::InvalidLayout("requirement_scope"));
    }
    // Экземпляр, ресурс и строки совместимости называет только устройство.
    if !device_kind && (instance != 0 || resource != 0) {
        return Err(PlanDecodeError::InvalidLayout("requirement_device"));
    }
    if device_kind == (selector_count == 0) {
        return Err(PlanDecodeError::InvalidLayout("requirement_selector"));
    }

    let (name, cursor) = parse_str(
        bytes,
        at + REQUIREMENT_HEADER_SIZE,
        name_len,
        "requirement_name",
    )?;
    let (source, end) = match kind {
        REQUIREMENT_KIND_EXPORT => {
            let (protocol, end) = parse_str(bytes, cursor, protocol_len, "requirement_protocol")?;
            let (scope, end) = parse_scope(bytes, end, scope_len)?;
            (
                RequirementSource::Export {
                    provider,
                    protocol,
                    scope,
                },
                end,
            )
        }
        REQUIREMENT_KIND_IRQ | REQUIREMENT_KIND_MMIO => {
            let end = parse_names(bytes, cursor, selector_count, "requirement_selector")?;
            let device = PlanDevice {
                selectors: bytes.get(cursor..end).ok_or(truncated(bytes, end))?,
                selector_count,
                instance,
                resource,
            };
            let source = if kind == REQUIREMENT_KIND_IRQ {
                RequirementSource::Irq(device)
            } else {
                RequirementSource::Mmio(device)
            };
            (source, end)
        }
        _ => (RequirementSource::Region, cursor),
    };

    Ok(ParsedRequirement {
        requirement: PlanRequirement { name, source },
        end,
    })
}

fn parse_names(
    bytes: &[u8],
    at: usize,
    names: u16,
    field: &'static str,
) -> Result<usize, PlanDecodeError> {
    let mut cursor = at;
    for _ in 0..names {
        let len = usize::from(read_u16(bytes, cursor)?);
        let (_, next) = parse_str(bytes, cursor + NAME_LEN_SIZE, len, field)?;
        cursor = next;
    }
    Ok(cursor)
}

fn parse_scope(bytes: &[u8], at: usize, len: usize) -> Result<(&str, usize), PlanDecodeError> {
    if len == 0 {
        return Ok(("", at));
    }
    parse_str(bytes, at, len, "requirement_scope")
}

fn parse_str<'a>(
    bytes: &'a [u8],
    at: usize,
    len: usize,
    field: &'static str,
) -> Result<(&'a str, usize), PlanDecodeError> {
    if len == 0 {
        return Err(PlanDecodeError::InvalidLayout(field));
    }
    let end = at + len;
    let text = bytes.get(at..end).ok_or(truncated(bytes, end))?;
    let text = core::str::from_utf8(text).map_err(|_| PlanDecodeError::InvalidLayout(field))?;
    Ok((text, end))
}

fn read_u16(bytes: &[u8], at: usize) -> Result<u16, PlanDecodeError> {
    let field = bytes
        .get(at..at + 2)
        .ok_or(truncated(bytes, at + 2))?
        .try_into()
        .map_err(|_| truncated(bytes, at + 2))?;
    Ok(u16::from_le_bytes(field))
}

fn read_u32(bytes: &[u8], at: usize) -> Result<u32, PlanDecodeError> {
    let field = bytes
        .get(at..at + 4)
        .ok_or(truncated(bytes, at + 4))?
        .try_into()
        .map_err(|_| truncated(bytes, at + 4))?;
    Ok(u32::from_le_bytes(field))
}

fn read_u64(bytes: &[u8], at: usize) -> Result<u64, PlanDecodeError> {
    let field = bytes
        .get(at..at + 8)
        .ok_or(truncated(bytes, at + 8))?
        .try_into()
        .map_err(|_| truncated(bytes, at + 8))?;
    Ok(u64::from_le_bytes(field))
}

fn truncated(bytes: &[u8], needed: usize) -> PlanDecodeError {
    PlanDecodeError::Truncated {
        needed,
        actual: bytes.len(),
    }
}

#[cfg(test)]
mod tests {
    use alloc::{vec, vec::Vec};

    use super::*;
    use crate::{
        ComponentSpec, DeviceSpec, PlanEncodeError, RequirementSourceSpec, RequirementSpec,
        encode_plan,
    };

    const RTC_SELECTORS: &[&str] = &["arm,pl031", "arm,primecell"];

    const ROUTER_EXPORTS: &[&str] = &["Namespace", "NamespaceAdmin"];
    const FILE_SYSTEM_EXPORTS: &[&str] = &["FileSystem"];

    const AREA: &str = "/boot/clock";

    const RTC_LINE: DeviceSpec<'static> = DeviceSpec {
        compatible: RTC_SELECTORS,
        instance: 0,
        resource: 0,
    };
    const RTC_WINDOW: DeviceSpec<'static> = DeviceSpec {
        compatible: RTC_SELECTORS,
        instance: 0,
        resource: 1,
    };

    fn rtc() -> ComponentSpec<'static> {
        ComponentSpec {
            executable: "/boot/driver-pl031",
            name: "clock",
            requirements: &[
                RequirementSpec {
                    name: "line",
                    source: RequirementSourceSpec::Irq(RTC_LINE),
                },
                RequirementSpec {
                    name: "window",
                    source: RequirementSourceSpec::Mmio(RTC_WINDOW),
                },
                RequirementSpec {
                    name: "names",
                    source: RequirementSourceSpec::Export {
                        provider: 1,
                        protocol: "Namespace",
                        scope: "",
                    },
                },
            ],
            exports: &[],
            memory_limit: 256 * 1024,
            restarts: 0,
        }
    }

    fn router() -> ComponentSpec<'static> {
        ComponentSpec {
            executable: "/boot/namespace-router",
            name: "router-a",
            requirements: &[],
            exports: ROUTER_EXPORTS,
            memory_limit: 8 * 1024 * 1024,
            restarts: 2,
        }
    }

    fn image_filesystem() -> ComponentSpec<'static> {
        ComponentSpec {
            executable: "/boot/filesystem-imagefs",
            name: "imagefs",
            requirements: &[RequirementSpec {
                name: "image",
                source: RequirementSourceSpec::Region,
            }],
            exports: FILE_SYSTEM_EXPORTS,
            memory_limit: 8 * 1024 * 1024,
            restarts: 1,
        }
    }

    fn importer<'a>(requirements: &'a [RequirementSpec<'a>]) -> ComponentSpec<'a> {
        ComponentSpec {
            executable: "/boot/importer",
            name: "importer",
            requirements,
            exports: &[],
            memory_limit: 64 * 1024,
            restarts: 0,
        }
    }

    fn session_requirement(
        name: &'static str,
        provider: u16,
        protocol: &'static str,
        scope: &'static str,
    ) -> RequirementSpec<'static> {
        RequirementSpec {
            name,
            source: RequirementSourceSpec::Export {
                provider,
                protocol,
                scope,
            },
        }
    }

    fn plan_bytes() -> Vec<u8> {
        encode_plan(&[rtc(), router(), image_filesystem()]).expect("encode plan")
    }

    fn first_requirement_at(component: &ComponentSpec<'_>) -> usize {
        PLAN_HEADER_SIZE + COMPONENT_HEADER_SIZE + component.executable.len() + component.name.len()
    }

    fn broken(bytes: &[u8], at: usize, field: &[u8]) -> Option<PlanDecodeError> {
        let mut broken = bytes.to_vec();
        broken[at..at + field.len()].copy_from_slice(field);
        decode_plan(&broken).err()
    }

    #[test]
    fn round_trip_exposes_every_component_and_its_params() {
        let bytes = plan_bytes();
        let plan = decode_plan(&bytes).expect("decode plan");
        assert_eq!(plan.len(), 3);

        let rtc = plan.component(0).expect("first component");
        assert_eq!(rtc.executable(), "/boot/driver-pl031");
        assert_eq!(rtc.name(), "clock");
        assert_eq!(rtc.memory_limit(), 256 * 1024);
        assert_eq!(rtc.restarts(), 0);
        assert_eq!(rtc.requirements().count(), 3);
        assert_eq!(rtc.exports().count(), 0);

        let router = plan.component(1).expect("second component");
        assert_eq!(router.name(), "router-a");
        assert_eq!(router.restarts(), 2);
        assert_eq!(router.requirements().count(), 0);
        assert_eq!(router.exports().collect::<Vec<_>>(), ROUTER_EXPORTS);

        let filesystem = plan.component(2).expect("third component");
        assert_eq!(filesystem.restarts(), 1);
        assert_eq!(
            filesystem.exports().collect::<Vec<_>>(),
            FILE_SYSTEM_EXPORTS
        );
        assert_eq!(
            filesystem.requirements().collect::<Vec<_>>(),
            vec![PlanRequirement {
                name: "image",
                source: RequirementSource::Region
            }]
        );
        assert!(plan.component(3).is_none());
    }

    #[test]
    fn round_trip_carries_every_requirement_by_its_name() {
        let bytes = plan_bytes();
        let plan = decode_plan(&bytes).expect("decode plan");
        let rtc = plan.component(0).expect("first component");

        assert_eq!(
            rtc.requirements()
                .map(|requirement| requirement.name)
                .collect::<Vec<_>>(),
            vec!["line", "window", "names"]
        );
        let Some(RequirementSource::Irq(line)) = rtc.requirement("line") else {
            panic!("`line` is an interrupt line")
        };
        assert_eq!(line.compatible().collect::<Vec<_>>(), RTC_SELECTORS);
        assert_eq!(line.resource, 0);
        let Some(RequirementSource::Mmio(window)) = rtc.requirement("window") else {
            panic!("`window` is an MMIO window")
        };
        assert_eq!(window.compatible().collect::<Vec<_>>(), RTC_SELECTORS);
        assert_eq!(window.resource, 1);
        assert_eq!(
            rtc.requirement("names"),
            Some(RequirementSource::Export {
                provider: 1,
                protocol: "Namespace",
                scope: ""
            })
        );
        assert_eq!(rtc.requirement("absent"), None);
    }

    #[test]
    fn round_trip_carries_the_provider_and_protocol_of_a_session_requirement() {
        let requirements = [
            session_requirement("names", 0, "Namespace", AREA),
            session_requirement("admin", 0, "NamespaceAdmin", ""),
        ];
        let bytes = encode_plan(&[router(), importer(&requirements)]).expect("encode plan");

        let plan = decode_plan(&bytes).expect("decode plan");
        let importer = plan.component(1).expect("second component");
        assert_eq!(
            importer.requirements().collect::<Vec<_>>(),
            vec![
                PlanRequirement {
                    name: "names",
                    source: RequirementSource::Export {
                        provider: 0,
                        protocol: "Namespace",
                        scope: AREA
                    }
                },
                PlanRequirement {
                    name: "admin",
                    source: RequirementSource::Export {
                        provider: 0,
                        protocol: "NamespaceAdmin",
                        scope: ""
                    }
                }
            ]
        );
    }

    #[test]
    fn round_trip_keeps_the_device_instance() {
        let second = DeviceSpec {
            instance: 1,
            ..RTC_LINE
        };
        let requirements = [
            RequirementSpec {
                name: "line",
                source: RequirementSourceSpec::Irq(second),
            },
            RequirementSpec {
                name: "window",
                source: RequirementSourceSpec::Mmio(second),
            },
        ];
        let bytes = encode_plan(&[importer(&requirements)]).expect("encode plan");

        let plan = decode_plan(&bytes).expect("decode plan");
        let component = plan.component(0).expect("component");
        let Some(RequirementSource::Irq(line)) = component.requirement("line") else {
            panic!("`line` is an interrupt line")
        };
        assert_eq!(line.instance, 1);
        let Some(RequirementSource::Mmio(window)) = component.requirement("window") else {
            panic!("`window` is an MMIO window")
        };
        assert_eq!(window.instance, 1);
    }

    #[test]
    fn a_memory_limit_off_the_page_boundary_survives_the_round_trip() {
        let component = ComponentSpec {
            memory_limit: 5_000,
            ..router()
        };
        let bytes = encode_plan(&[component]).expect("encode plan");
        let plan = decode_plan(&bytes).expect("decode plan");

        assert_eq!(plan.component(0).expect("component").memory_limit(), 5_000);
    }

    #[test]
    fn round_trip_keeps_two_instances_of_one_program_apart() {
        let second = ComponentSpec {
            name: "router-b",
            ..router()
        };
        let bytes = encode_plan(&[router(), second]).expect("encode plan");

        let plan = decode_plan(&bytes).expect("decode plan");
        let first = plan.component(0).expect("first instance");
        let second = plan.component(1).expect("second instance");
        assert_eq!(first.executable(), second.executable());
        assert_eq!(first.name(), "router-a");
        assert_eq!(second.name(), "router-b");
    }

    #[test]
    fn a_program_record_is_named_by_the_directory_that_holds_it() {
        assert_eq!(
            component_name("/boot/namespace-router/program"),
            "namespace-router"
        );
        assert_eq!(component_name("/boot/namespace-router/mounts"), "mounts");
        assert_eq!(component_name("/boot/rootkeeper"), "rootkeeper");
        assert_eq!(component_name("/program"), "program");
        assert_eq!(component_name("program"), "program");
    }

    #[test]
    fn decode_rejects_foreign_magic() {
        let mut bytes = plan_bytes();
        bytes[0] = b'X';
        assert!(matches!(
            decode_plan(&bytes),
            Err(PlanDecodeError::InvalidMagic(_))
        ));
    }

    #[test]
    fn decode_rejects_another_version() {
        let mut bytes = plan_bytes();
        bytes[8..10].copy_from_slice(&9u16.to_le_bytes());
        assert_eq!(
            decode_plan(&bytes).expect_err("version must match"),
            PlanDecodeError::InvalidVersion(9)
        );
    }

    #[test]
    fn decode_rejects_a_composition_without_components() {
        let mut bytes = plan_bytes();
        bytes[10..12].copy_from_slice(&0u16.to_le_bytes());
        assert_eq!(
            decode_plan(&bytes).expect_err("composition carries components"),
            PlanDecodeError::InvalidLayout("component_count")
        );
    }

    #[test]
    fn decode_rejects_a_truncated_record() {
        let bytes = plan_bytes();
        for length in [0, PLAN_HEADER_SIZE, bytes.len() - 1] {
            assert!(matches!(
                decode_plan(&bytes[..length]),
                Err(PlanDecodeError::Truncated { .. })
            ));
        }
    }

    #[test]
    fn decode_rejects_bytes_past_the_composition() {
        let mut bytes = plan_bytes();
        bytes.push(0);
        assert_eq!(
            decode_plan(&bytes).expect_err("trailing bytes must fail"),
            PlanDecodeError::InvalidLayout("trailing_bytes")
        );
    }

    #[test]
    fn decode_rejects_values_that_no_component_can_run_with() {
        let bytes = plan_bytes();
        let at = PLAN_HEADER_SIZE + COMPONENT_MEMORY_LIMIT;
        assert_eq!(
            broken(&bytes, at, &0u64.to_le_bytes()),
            Some(PlanDecodeError::InvalidLayout("memory_limit"))
        );
    }

    #[test]
    fn decode_rejects_a_reserved_field_in_use() {
        let bytes = plan_bytes();
        for at in [
            12,
            PLAN_HEADER_SIZE + COMPONENT_RESERVED,
            first_requirement_at(&rtc()) + REQUIREMENT_RESERVED,
        ] {
            assert_eq!(
                broken(&bytes, at, &[1]),
                Some(PlanDecodeError::InvalidLayout("reserved"))
            );
        }
    }

    #[test]
    fn decode_rejects_a_string_that_is_not_utf8() {
        let bytes = plan_bytes();
        let executable_at = PLAN_HEADER_SIZE + COMPONENT_HEADER_SIZE;
        assert_eq!(
            broken(&bytes, executable_at, &[0xFF]),
            Some(PlanDecodeError::InvalidLayout("executable"))
        );
    }

    #[test]
    fn decode_rejects_a_requirement_of_an_unknown_kind() {
        let bytes = plan_bytes();
        let kind_at = first_requirement_at(&rtc()) + REQUIREMENT_KIND;
        for kind in [4, 9] {
            assert_eq!(
                broken(&bytes, kind_at, &[kind]),
                Some(PlanDecodeError::InvalidLayout("requirement_kind"))
            );
        }
    }

    #[test]
    fn decode_rejects_a_requirement_name_repeated_in_a_record() {
        let region = RequirementSpec {
            name: "image",
            source: RequirementSourceSpec::Region,
        };
        let bytes = encode_plan(&[importer(&[region, region])]).expect("encode plan");

        assert_eq!(
            decode_plan(&bytes),
            Err(PlanDecodeError::InvalidLayout("requirement_name"))
        );
    }

    #[test]
    fn decode_rejects_a_requirement_named_log() {
        let requirement = RequirementSpec {
            name: LOG_REQUIREMENT,
            source: RequirementSourceSpec::Region,
        };
        let bytes = encode_plan(&[importer(&[requirement])]).expect("encode plan");

        assert_eq!(
            decode_plan(&bytes),
            Err(PlanDecodeError::InvalidLayout("requirement_name"))
        );
    }

    #[test]
    fn decode_rejects_a_session_requirement_whose_provider_is_outside_the_plan() {
        let requirements = [session_requirement("names", 2, "Namespace", "")];
        let bytes = encode_plan(&[importer(&requirements), router()]).expect("encode plan");

        assert_eq!(
            decode_plan(&bytes),
            Err(PlanDecodeError::InvalidLayout("requirement_provider"))
        );
    }

    #[test]
    fn decode_rejects_a_session_requirement_provided_by_its_own_record() {
        let requirements = [session_requirement("names", 0, "Namespace", "")];
        let component = ComponentSpec {
            requirements: &requirements,
            ..router()
        };
        let bytes = encode_plan(&[component]).expect("encode plan");

        assert_eq!(
            decode_plan(&bytes),
            Err(PlanDecodeError::InvalidLayout("requirement_provider"))
        );
    }

    #[test]
    fn decode_rejects_a_session_requirement_whose_provider_does_not_export_its_protocol() {
        let requirements = [session_requirement("files", 1, "FileSystem", "")];
        let bytes = encode_plan(&[importer(&requirements), router()]).expect("encode plan");

        assert_eq!(
            decode_plan(&bytes),
            Err(PlanDecodeError::InvalidLayout("requirement_protocol"))
        );
    }

    #[test]
    fn decode_rejects_session_fields_on_a_device_requirement() {
        let bytes = plan_bytes();
        let line_at = first_requirement_at(&rtc());
        assert_eq!(
            broken(&bytes, line_at + REQUIREMENT_PROVIDER, &1u16.to_le_bytes()),
            Some(PlanDecodeError::InvalidLayout("requirement_provider"))
        );
        assert_eq!(
            broken(
                &bytes,
                line_at + REQUIREMENT_PROTOCOL_LEN,
                &1u16.to_le_bytes()
            ),
            Some(PlanDecodeError::InvalidLayout("requirement_protocol"))
        );
        assert_eq!(
            broken(&bytes, line_at + REQUIREMENT_SCOPE_LEN, &1u16.to_le_bytes()),
            Some(PlanDecodeError::InvalidLayout("requirement_scope"))
        );
    }

    #[test]
    fn decode_rejects_device_fields_on_a_region_requirement() {
        let region = [RequirementSpec {
            name: "image",
            source: RequirementSourceSpec::Region,
        }];
        let component = importer(&region);
        let bytes = encode_plan(&[component]).expect("encode plan");
        let requirement_at = first_requirement_at(&component);
        for field in [REQUIREMENT_INSTANCE, REQUIREMENT_RESOURCE] {
            assert_eq!(
                broken(&bytes, requirement_at + field, &1u16.to_le_bytes()),
                Some(PlanDecodeError::InvalidLayout("requirement_device"))
            );
        }
        assert_eq!(
            broken(
                &bytes,
                requirement_at + REQUIREMENT_SELECTOR_COUNT,
                &1u16.to_le_bytes()
            ),
            Some(PlanDecodeError::InvalidLayout("requirement_selector"))
        );
    }

    #[test]
    fn decode_rejects_a_scope_past_the_bound() {
        let requirements = [session_requirement("names", 1, "Namespace", AREA)];
        let component = importer(&requirements);
        let bytes = encode_plan(&[component, router()]).expect("encode plan");
        let scope_at = first_requirement_at(&component) + REQUIREMENT_SCOPE_LEN;
        let past = u16::try_from(SESSION_SCOPE_MAX + 1).expect("bound fits the field");

        assert_eq!(
            broken(&bytes, scope_at, &past.to_le_bytes()),
            Some(PlanDecodeError::InvalidLayout("requirement_scope"))
        );
    }

    #[test]
    fn decode_rejects_a_device_requirement_without_selectors() {
        let bytes = plan_bytes();
        let count_at = first_requirement_at(&rtc()) + REQUIREMENT_SELECTOR_COUNT;
        assert_eq!(
            broken(&bytes, count_at, &0u16.to_le_bytes()),
            Some(PlanDecodeError::InvalidLayout("requirement_selector"))
        );
    }

    #[test]
    fn encode_rejects_a_composition_that_cannot_run() {
        let zero_memory = ComponentSpec {
            memory_limit: 0,
            ..router()
        };
        assert_eq!(
            encode_plan(&[zero_memory]).expect_err("memory limit must be non-zero"),
            PlanEncodeError::ZeroMemory
        );

        let no_executable = ComponentSpec {
            executable: "",
            ..router()
        };
        assert_eq!(
            encode_plan(&[no_executable]).expect_err("executable must be non-empty"),
            PlanEncodeError::EmptyString
        );

        let nameless = ComponentSpec {
            name: "",
            ..router()
        };
        assert_eq!(
            encode_plan(&[nameless]).expect_err("instance name must be non-empty"),
            PlanEncodeError::EmptyString
        );

        assert_eq!(
            encode_plan(&[]).expect_err("composition carries components"),
            PlanEncodeError::EmptyComposition
        );

        let nameless_requirement = [session_requirement("", 1, "Namespace", "")];
        assert_eq!(
            encode_plan(&[importer(&nameless_requirement)]).expect_err("requirements have names"),
            PlanEncodeError::EmptyString
        );

        let no_protocol = [session_requirement("names", 1, "", "")];
        assert_eq!(
            encode_plan(&[importer(&no_protocol)])
                .expect_err("a session requirement names a protocol"),
            PlanEncodeError::EmptyString
        );

        let no_selectors = [RequirementSpec {
            name: "line",
            source: RequirementSourceSpec::Irq(DeviceSpec {
                compatible: &[],
                ..RTC_LINE
            }),
        }];
        assert_eq!(
            encode_plan(&[importer(&no_selectors)])
                .expect_err("a device requirement names compatibles"),
            PlanEncodeError::EmptyString
        );

        let long_scope = "/".repeat(SESSION_SCOPE_MAX + 1);
        let wide_scope = [RequirementSpec {
            name: "names",
            source: RequirementSourceSpec::Export {
                provider: 1,
                protocol: "Namespace",
                scope: &long_scope,
            },
        }];
        assert_eq!(
            encode_plan(&[importer(&wide_scope)]).expect_err("scope fits its bound"),
            PlanEncodeError::ScopeTooLong
        );

        let nameless_export = ComponentSpec {
            exports: &[""],
            ..router()
        };
        assert_eq!(
            encode_plan(&[nameless_export]).expect_err("exports name protocols"),
            PlanEncodeError::EmptyString
        );
    }
}
