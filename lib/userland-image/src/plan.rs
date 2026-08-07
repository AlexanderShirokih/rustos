use syscall::Timeout;

/// Путь записи образа, из которой rootkeeper читает план загрузочной композиции.
pub const BOOT_PLAN_PATH: &str = "/boot/rootkeeper.plan";

pub const BOOT_PLAN_MAGIC: [u8; 8] = *b"RKPLAN\0\0";
pub const BOOT_PLAN_VERSION: u16 = 2;

pub(crate) const PLAN_HEADER_SIZE: usize = 16;
pub(crate) const COMPONENT_HEADER_SIZE: usize = 40;
pub(crate) const SLOT_HEADER_SIZE: usize = 12;

pub(crate) const SLOT_KIND_IMAGE: u8 = 0;
pub(crate) const SLOT_KIND_IRQ: u8 = 1;
pub(crate) const SLOT_KIND_MMIO: u8 = 2;
pub(crate) const SLOT_KIND_SERVICE: u8 = 3;

const INDEX_SIZE: usize = 2;

/// Устройство описания платформы, названное слотом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanDevice<'a> {
    /// Строка совместимости устройства.
    pub compatible: &'a str,
    /// Номер экземпляра среди устройств той же совместимости.
    pub instance: u16,
    /// Номер ресурса устройства: линии либо окна.
    pub resource: u16,
}

/// Источник capability, которую supervisor кладёт в слот компонента.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotSource<'a> {
    /// Регион userland-образа целиком.
    Image,
    /// Линия прерывания устройства.
    Irq(PlanDevice<'a>),
    /// MMIO-окно устройства.
    Mmio(PlanDevice<'a>),
    /// Сессия service-порта компонента с номером `provider` в плане.
    Service { provider: u16 },
}

/// Компонент загрузочной композиции.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanComponent<'a> {
    id: &'a str,
    program: &'a str,
    config: Option<&'a str>,
    slots: &'a [u8],
    slot_count: u16,
    requires: &'a [u8],
    after: &'a [u8],
    budget_pages: u64,
    startup_timeout: Timeout,
    restart_limit: u32,
    critical: bool,
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

/// Слоты компонента в порядке плана.
#[derive(Debug, Clone)]
pub struct PlanSlots<'a> {
    bytes: &'a [u8],
    cursor: usize,
    remaining: u16,
}

/// Номера компонентов, на которые ссылается ребро.
#[derive(Debug, Clone)]
pub struct PlanEdges<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanDecodeError {
    InvalidMagic([u8; 8]),
    InvalidVersion(u16),
    Truncated { needed: usize, actual: usize },
    InvalidLayout(&'static str),
}

struct ParsedComponent<'a> {
    component: PlanComponent<'a>,
    end: usize,
}

struct ParsedSlot<'a> {
    source: SlotSource<'a>,
    end: usize,
}

/// Декодирует запись плана. Буфер разбирается целиком: хвост сверх плана
/// отвергается.
pub fn decode_plan(bytes: &[u8]) -> Result<BootPlan<'_>, PlanDecodeError> {
    BootPlan::decode(bytes)
}

impl<'a> PlanComponent<'a> {
    /// Имя компонента в журнале композиции.
    #[must_use]
    pub const fn id(&self) -> &'a str {
        self.id
    }

    /// Запись образа с программой компонента.
    #[must_use]
    pub const fn program(&self) -> &'a str {
        self.program
    }

    /// Запись образа с конфигурацией компонента.
    #[must_use]
    pub const fn config(&self) -> Option<&'a str> {
        self.config
    }

    /// Число слотов capability компонента.
    #[must_use]
    pub const fn slot_count(&self) -> u16 {
        self.slot_count
    }

    /// Источники слотов компонента в порядке плана.
    #[must_use]
    pub const fn slots(&self) -> PlanSlots<'a> {
        PlanSlots {
            bytes: self.slots,
            cursor: 0,
            remaining: self.slot_count,
        }
    }

    /// Компоненты, чей сервис требуется этому.
    #[must_use]
    pub const fn requires(&self) -> PlanEdges<'a> {
        PlanEdges {
            bytes: self.requires,
            cursor: 0,
        }
    }

    /// Компоненты, которые поднимаются раньше этого без обмена с ним.
    #[must_use]
    pub const fn after(&self) -> PlanEdges<'a> {
        PlanEdges {
            bytes: self.after,
            cursor: 0,
        }
    }

    /// Бюджет страниц, отрезаемый компоненту.
    #[must_use]
    pub const fn budget_pages(&self) -> u64 {
        self.budget_pages
    }

    /// Конечный срок доставки startup-кадра.
    #[must_use]
    pub const fn startup_timeout(&self) -> Timeout {
        self.startup_timeout
    }

    /// Предел числа запусков: первый запуск считается.
    #[must_use]
    pub const fn restart_limit(&self) -> u32 {
        self.restart_limit
    }

    /// Уход критического компонента останавливает композицию.
    #[must_use]
    pub const fn critical(&self) -> bool {
        self.critical
    }
}

impl<'a> BootPlan<'a> {
    /// Число компонентов композиции.
    #[must_use]
    pub const fn len(&self) -> u16 {
        self.count
    }

    /// Композиция плана пуста не бывает: декодер отвергает такую запись.
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
        let magic: [u8; 8] = header[..8].try_into().map_err(|_| truncated(bytes, 8))?;
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
            cursor = parse_component(bytes, cursor, count)?.end;
        }
        if cursor != bytes.len() {
            return Err(PlanDecodeError::InvalidLayout("trailing_bytes"));
        }
        Ok(Self { bytes, count })
    }
}

impl<'a> Iterator for PlanComponents<'a> {
    type Item = PlanComponent<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        // Запись проверена декодированием плана целиком: повторный разбор
        // читает те же байты.
        let parsed = parse_component(self.bytes, self.cursor, u16::MAX).ok()?;
        self.remaining -= 1;
        self.cursor = parsed.end;
        Some(parsed.component)
    }
}

impl<'a> Iterator for PlanSlots<'a> {
    type Item = SlotSource<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let parsed = parse_slot(self.bytes, self.cursor, u16::MAX).ok()?;
        self.remaining -= 1;
        self.cursor = parsed.end;
        Some(parsed.source)
    }
}

impl Iterator for PlanEdges<'_> {
    type Item = u16;

    fn next(&mut self) -> Option<Self::Item> {
        let index = read_u16(self.bytes, self.cursor).ok()?;
        self.cursor += INDEX_SIZE;
        Some(index)
    }
}

fn parse_component(
    bytes: &[u8],
    at: usize,
    count: u16,
) -> Result<ParsedComponent<'_>, PlanDecodeError> {
    let header = bytes
        .get(at..at + COMPONENT_HEADER_SIZE)
        .ok_or(truncated(bytes, at + COMPONENT_HEADER_SIZE))?;
    let restart_limit = read_u32(header, 0)?;
    if restart_limit == 0 {
        return Err(PlanDecodeError::InvalidLayout("restart_limit"));
    }
    let critical = match header[4] {
        0 => false,
        1 => true,
        _ => return Err(PlanDecodeError::InvalidLayout("critical")),
    };
    if header[5] != 0 || read_u16(header, 18)? != 0 || read_u32(header, 20)? != 0 {
        return Err(PlanDecodeError::InvalidLayout("reserved"));
    }
    let id_len = usize::from(read_u16(header, 6)?);
    let program_len = usize::from(read_u16(header, 8)?);
    let config_len = usize::from(read_u16(header, 10)?);
    let slot_count = read_u16(header, 12)?;
    let requires_count = read_u16(header, 14)?;
    let after_count = read_u16(header, 16)?;
    let budget_pages = read_u64(header, 24)?;
    if budget_pages == 0 {
        return Err(PlanDecodeError::InvalidLayout("budget_pages"));
    }
    let startup_timeout = Timeout::from_ns(read_u64(header, 32)?);
    if startup_timeout == Timeout::POLL || startup_timeout == Timeout::INFINITE {
        return Err(PlanDecodeError::InvalidLayout("startup_timeout"));
    }

    let cursor = at + COMPONENT_HEADER_SIZE;
    let (id, cursor) = parse_str(bytes, cursor, id_len, "id")?;
    let (program, cursor) = parse_str(bytes, cursor, program_len, "program")?;
    let (config, mut cursor) = match config_len {
        0 => (None, cursor),
        len => {
            let (config, cursor) = parse_str(bytes, cursor, len, "config")?;
            (Some(config), cursor)
        }
    };

    let slots_at = cursor;
    for _ in 0..slot_count {
        cursor = parse_slot(bytes, cursor, count)?.end;
    }
    let slots = bytes
        .get(slots_at..cursor)
        .ok_or(truncated(bytes, cursor))?;

    let requires = parse_edges(bytes, cursor, requires_count, count)?;
    cursor += requires.len();
    let after = parse_edges(bytes, cursor, after_count, count)?;
    cursor += after.len();

    Ok(ParsedComponent {
        component: PlanComponent {
            id,
            program,
            config,
            slots,
            slot_count,
            requires,
            after,
            budget_pages,
            startup_timeout,
            restart_limit,
            critical,
        },
        end: cursor,
    })
}

fn parse_slot(bytes: &[u8], at: usize, count: u16) -> Result<ParsedSlot<'_>, PlanDecodeError> {
    let header = bytes
        .get(at..at + SLOT_HEADER_SIZE)
        .ok_or(truncated(bytes, at + SLOT_HEADER_SIZE))?;
    let kind = header[0];
    if header[1] != 0 || read_u16(header, 10)? != 0 {
        return Err(PlanDecodeError::InvalidLayout("reserved"));
    }
    let provider = read_u16(header, 2)?;
    let instance = read_u16(header, 4)?;
    let resource = read_u16(header, 6)?;
    let selector_len = usize::from(read_u16(header, 8)?);

    let cursor = at + SLOT_HEADER_SIZE;
    let device_kind = matches!(kind, SLOT_KIND_IRQ | SLOT_KIND_MMIO);
    if !device_kind && !matches!(kind, SLOT_KIND_IMAGE | SLOT_KIND_SERVICE) {
        return Err(PlanDecodeError::InvalidLayout("slot_kind"));
    }
    if !device_kind && (instance != 0 || resource != 0 || selector_len != 0) {
        return Err(PlanDecodeError::InvalidLayout("slot_device"));
    }
    if kind != SLOT_KIND_SERVICE && provider != 0 {
        return Err(PlanDecodeError::InvalidLayout("slot_provider"));
    }

    let (source, end) = match kind {
        SLOT_KIND_IMAGE => (SlotSource::Image, cursor),
        SLOT_KIND_SERVICE => {
            if provider >= count {
                return Err(PlanDecodeError::InvalidLayout("slot_provider"));
            }
            (SlotSource::Service { provider }, cursor)
        }
        SLOT_KIND_IRQ | SLOT_KIND_MMIO => {
            let (compatible, end) = parse_str(bytes, cursor, selector_len, "slot_selector")?;
            let device = PlanDevice {
                compatible,
                instance,
                resource,
            };
            let source = if kind == SLOT_KIND_IRQ {
                SlotSource::Irq(device)
            } else {
                SlotSource::Mmio(device)
            };
            (source, end)
        }
        _ => return Err(PlanDecodeError::InvalidLayout("slot_kind")),
    };

    Ok(ParsedSlot { source, end })
}

fn parse_edges(bytes: &[u8], at: usize, edges: u16, count: u16) -> Result<&[u8], PlanDecodeError> {
    let end = at + usize::from(edges) * INDEX_SIZE;
    let raw = bytes.get(at..end).ok_or(truncated(bytes, end))?;
    for index in 0..usize::from(edges) {
        if read_u16(raw, index * INDEX_SIZE)? >= count {
            return Err(PlanDecodeError::InvalidLayout("edge"));
        }
    }
    Ok(raw)
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
    use crate::{ComponentSpec, PlanEncodeError, encode_plan};

    const CONFIG: &str = "/boot/namespace.mounts";
    const RTC_SELECTOR: &str = "arm,pl031";

    fn router() -> ComponentSpec<'static> {
        ComponentSpec {
            id: "filesystem",
            program: "/boot/namespace-router",
            config: Some(CONFIG),
            slots: &[SlotSource::Image],
            requires: &[],
            after: &[],
            budget_pages: 2_048,
            startup_timeout: Timeout::from_millis(2_000),
            restart_limit: 3,
            critical: true,
        }
    }

    fn rtc() -> ComponentSpec<'static> {
        ComponentSpec {
            id: "rtc",
            program: "/boot/driver-pl031",
            config: None,
            slots: &[
                SlotSource::Irq(PlanDevice {
                    compatible: RTC_SELECTOR,
                    instance: 0,
                    resource: 0,
                }),
                SlotSource::Mmio(PlanDevice {
                    compatible: RTC_SELECTOR,
                    instance: 0,
                    resource: 0,
                }),
                SlotSource::Service { provider: 0 },
            ],
            requires: &[0],
            after: &[0],
            budget_pages: 64,
            startup_timeout: Timeout::from_millis(1_000),
            restart_limit: 1,
            critical: false,
        }
    }

    fn plan_bytes() -> Vec<u8> {
        encode_plan(&[router(), rtc()]).expect("encode plan")
    }

    #[test]
    fn round_trip_exposes_every_component_and_its_params() {
        let bytes = plan_bytes();
        let plan = decode_plan(&bytes).expect("decode plan");
        assert_eq!(plan.len(), 2);

        let router = plan.component(0).expect("first component");
        assert_eq!(router.id(), "filesystem");
        assert_eq!(router.program(), "/boot/namespace-router");
        assert_eq!(router.config(), Some(CONFIG));
        assert_eq!(router.budget_pages(), 2_048);
        assert_eq!(router.startup_timeout(), Timeout::from_millis(2_000));
        assert_eq!(router.restart_limit(), 3);
        assert!(router.critical());
        assert_eq!(router.slots().collect::<Vec<_>>(), vec![SlotSource::Image]);
        assert_eq!(router.requires().count(), 0);

        let rtc = plan.component(1).expect("second component");
        assert_eq!(rtc.id(), "rtc");
        assert_eq!(rtc.config(), None);
        assert!(!rtc.critical());
        assert_eq!(rtc.slot_count(), 3);
        assert_eq!(
            rtc.slots().collect::<Vec<_>>(),
            vec![
                SlotSource::Irq(PlanDevice {
                    compatible: RTC_SELECTOR,
                    instance: 0,
                    resource: 0,
                }),
                SlotSource::Mmio(PlanDevice {
                    compatible: RTC_SELECTOR,
                    instance: 0,
                    resource: 0,
                }),
                SlotSource::Service { provider: 0 },
            ]
        );
        assert_eq!(rtc.requires().collect::<Vec<_>>(), vec![0]);
        assert_eq!(rtc.after().collect::<Vec<_>>(), vec![0]);
    }

    #[test]
    fn round_trip_keeps_a_component_without_a_config_record() {
        let bytes = encode_plan(&[rtc()]).expect("encode plan");

        let component = decode_plan(&bytes)
            .expect("decode plan")
            .component(0)
            .expect("component");
        assert_eq!(component.config(), None);
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
        bytes[8..10].copy_from_slice(&1u16.to_le_bytes());
        assert_eq!(
            decode_plan(&bytes).expect_err("version must match"),
            PlanDecodeError::InvalidVersion(1)
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
        let field = |offset: usize, value: u64| {
            let mut bytes = plan_bytes();
            let at = PLAN_HEADER_SIZE + offset;
            bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
            decode_plan(&bytes).expect_err("component must be runnable")
        };

        assert_eq!(field(24, 0), PlanDecodeError::InvalidLayout("budget_pages"));
        assert_eq!(
            field(32, Timeout::POLL.raw()),
            PlanDecodeError::InvalidLayout("startup_timeout")
        );
        assert_eq!(
            field(32, Timeout::INFINITE.raw()),
            PlanDecodeError::InvalidLayout("startup_timeout")
        );

        let mut bytes = plan_bytes();
        bytes[PLAN_HEADER_SIZE..PLAN_HEADER_SIZE + 4].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(
            decode_plan(&bytes).expect_err("component must launch at least once"),
            PlanDecodeError::InvalidLayout("restart_limit")
        );
    }

    #[test]
    fn decode_rejects_a_flag_outside_its_values() {
        let mut bytes = plan_bytes();
        bytes[PLAN_HEADER_SIZE + 4] = 2;
        assert_eq!(
            decode_plan(&bytes).expect_err("critical is a flag"),
            PlanDecodeError::InvalidLayout("critical")
        );
    }

    #[test]
    fn decode_rejects_a_reserved_field_in_use() {
        let mut bytes = plan_bytes();
        bytes[PLAN_HEADER_SIZE + 5] = 1;
        assert_eq!(
            decode_plan(&bytes).expect_err("reserved bytes are zero"),
            PlanDecodeError::InvalidLayout("reserved")
        );
    }

    #[test]
    fn decode_rejects_a_string_that_is_not_utf8() {
        let mut bytes = plan_bytes();
        let id_at = PLAN_HEADER_SIZE + COMPONENT_HEADER_SIZE;
        bytes[id_at] = 0xFF;
        assert_eq!(
            decode_plan(&bytes).expect_err("strings are UTF-8"),
            PlanDecodeError::InvalidLayout("id")
        );
    }

    #[test]
    fn decode_rejects_a_slot_of_an_unknown_kind() {
        let bytes = encode_plan(&[rtc()]).expect("encode plan");
        let slot_at = bytes.len() - 3 * SLOT_HEADER_SIZE - 2 * RTC_SELECTOR.len() - 2 * INDEX_SIZE;
        let mut broken = bytes;
        broken[slot_at] = 9;
        assert_eq!(
            decode_plan(&broken).expect_err("slot kinds are closed"),
            PlanDecodeError::InvalidLayout("slot_kind")
        );
    }

    #[test]
    fn decode_rejects_an_edge_outside_the_composition() {
        let bytes = plan_bytes();
        let edge_at = bytes.len() - INDEX_SIZE;
        let mut broken = bytes;
        broken[edge_at..].copy_from_slice(&5u16.to_le_bytes());
        assert_eq!(
            decode_plan(&broken).expect_err("edges name components of the plan"),
            PlanDecodeError::InvalidLayout("edge")
        );
    }

    #[test]
    fn encode_rejects_a_composition_that_cannot_run() {
        let zero_budget = ComponentSpec {
            budget_pages: 0,
            ..router()
        };
        assert_eq!(
            encode_plan(&[zero_budget]).expect_err("budget must be non-zero"),
            PlanEncodeError::ZeroBudget
        );

        let no_launches = ComponentSpec {
            restart_limit: 0,
            ..router()
        };
        assert_eq!(
            encode_plan(&[no_launches]).expect_err("component must launch once"),
            PlanEncodeError::ZeroRestartLimit
        );

        let unbounded = ComponentSpec {
            startup_timeout: Timeout::INFINITE,
            ..router()
        };
        assert_eq!(
            encode_plan(&[unbounded]).expect_err("startup timeout must be finite"),
            PlanEncodeError::UnboundedStartupTimeout
        );

        let nameless = ComponentSpec { id: "", ..router() };
        assert_eq!(
            encode_plan(&[nameless]).expect_err("id must be non-empty"),
            PlanEncodeError::EmptyString
        );

        assert_eq!(
            encode_plan(&[]).expect_err("composition carries components"),
            PlanEncodeError::EmptyComposition
        );

        let unknown_provider = ComponentSpec {
            slots: &[SlotSource::Service { provider: 4 }],
            ..router()
        };
        assert_eq!(
            encode_plan(&[unknown_provider]).expect_err("provider is a component of the plan"),
            PlanEncodeError::IndexOutOfRange
        );

        let unknown_edge = ComponentSpec {
            requires: &[7],
            ..router()
        };
        assert_eq!(
            encode_plan(&[unknown_edge]).expect_err("edges name components of the plan"),
            PlanEncodeError::IndexOutOfRange
        );
    }
}
