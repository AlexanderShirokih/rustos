use syscall::Timeout;
use userland::entry_name;

/// Путь записи образа, из которой rootkeeper читает план загрузочной композиции.
pub const BOOT_PLAN_PATH: &str = "/boot/rootkeeper/plan";

/// Каталог образа с записями ступени 1: одна запись плана на компонент, и владелец политики
/// загрузки перечисляет их своей файловой сессией.
pub const BOOT_STAGE_ONE_DIRECTORY: &str = "/boot/init/stage1";

/// Имя записи с программой в каталоге компонента.
pub const COMPONENT_PROGRAM_NAME: &str = "program";

/// Имя компонента, которому принадлежит запись: каталог записи с программой.
/// Запись вне каталога компонента названа собственным именем.
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
pub(crate) const COMPONENT_HEADER_SIZE: usize = 40;
pub(crate) const SLOT_HEADER_SIZE: usize = 12;

pub(crate) const SLOT_KIND_USERLAND: u8 = 0;
pub(crate) const SLOT_KIND_IRQ: u8 = 1;
pub(crate) const SLOT_KIND_MMIO: u8 = 2;
pub(crate) const SLOT_KIND_SERVICE: u8 = 3;
pub(crate) const SLOT_KIND_DIRECTORY: u8 = 4;
pub(crate) const SLOT_KIND_SUPERVISOR: u8 = 5;
pub(crate) const SLOT_KIND_CONTROL: u8 = 6;

pub(crate) const EXHAUSTED_HALT: u8 = 0;
pub(crate) const EXHAUSTED_DETACH: u8 = 1;

pub(crate) const FLAG_ESTABLISHES_SESSIONS: u8 = 1;

pub(crate) const NAME_LEN_SIZE: usize = 2;

/// Исход компонента, исчерпавшего свои запуски.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnExhausted {
    /// Останавливает композицию.
    Halt,
    /// Снимается с композиции, а та остаётся поднятой без него.
    Detach,
}

/// Устройство описания платформы, названное слотом: набор совместимостей, с
/// которыми работает драйвер, вместе с номерами экземпляра и ресурса.
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

/// Источник capability, которую supervisor кладёт в слот компонента.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotSource<'a> {
    /// Read-only регион userland-образа.
    Userland,
    /// Линия прерывания устройства.
    Irq(PlanDevice<'a>),
    /// MMIO-окно устройства.
    Mmio(PlanDevice<'a>),
    /// Сессия service-порта экземпляра `provider`.
    Service { provider: &'a str },
    /// Сессия с каталогом компонента в роли корня. `provider` называет экземпляр,
    /// у которого rootkeeper устанавливает её областью в этом каталоге.
    Directory { provider: &'a str },
    /// Клиентская capability supervisor, поднявшего компонент.
    Supervisor,
    /// Клиентская capability control-порта экземпляра `provider`.
    Control { provider: &'a str },
}

/// Компонент загрузочной композиции.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanComponent<'a> {
    executable: &'a str,
    name: &'a str,
    slots: &'a [u8],
    slot_count: u16,
    after: &'a [u8],
    after_count: u16,
    memory_pages: u64,
    startup_timeout: Timeout,
    launches: u32,
    on_exhausted: OnExhausted,
    establishes_sessions: bool,
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
    /// Запись образа с программой компонента. Одну программу композиция
    /// поднимает несколькими экземплярами.
    #[must_use]
    pub const fn executable(&self) -> &'a str {
        self.executable
    }

    /// Имя экземпляра: им композиция называет компонент, и под ним его
    /// процесс и строки журнала.
    #[must_use]
    pub const fn name(&self) -> &'a str {
        self.name
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

    /// Имена экземпляров, которые поднимаются раньше этого без обмена с ним.
    #[must_use]
    pub const fn after(&self) -> PlanNames<'a> {
        PlanNames {
            bytes: self.after,
            cursor: 0,
            remaining: self.after_count,
        }
    }

    /// Предел памяти компонента в страницах: об него метрится всё, что процесс
    /// занимает.
    #[must_use]
    pub const fn memory_pages(&self) -> u64 {
        self.memory_pages
    }

    /// Конечный срок доставки startup-кадра.
    #[must_use]
    pub const fn startup_timeout(&self) -> Timeout {
        self.startup_timeout
    }

    /// Предел числа запусков: первый запуск считается.
    #[must_use]
    pub const fn launches(&self) -> u32 {
        self.launches
    }

    /// Исход компонента, исчерпавшего свои запуски.
    #[must_use]
    pub const fn on_exhausted(&self) -> OnExhausted {
        self.on_exhausted
    }

    /// Компонент заводит сессии своего service-порта сам: supervisor просит их
    /// операцией `ComponentSessions::open_session` на его control-порту.
    #[must_use]
    pub const fn establishes_sessions(&self) -> bool {
        self.establishes_sessions
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
        let parsed = parse_component(self.bytes, self.cursor).ok()?;
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
        let parsed = parse_slot(self.bytes, self.cursor).ok()?;
        self.remaining -= 1;
        self.cursor = parsed.end;
        Some(parsed.source)
    }
}

fn parse_component(bytes: &[u8], at: usize) -> Result<ParsedComponent<'_>, PlanDecodeError> {
    let header = bytes
        .get(at..at + COMPONENT_HEADER_SIZE)
        .ok_or(truncated(bytes, at + COMPONENT_HEADER_SIZE))?;
    let launches = read_u32(header, 0)?;
    if launches == 0 {
        return Err(PlanDecodeError::InvalidLayout("launches"));
    }
    let on_exhausted = match *header.get(4).ok_or(truncated(bytes, at + 5))? {
        EXHAUSTED_HALT => OnExhausted::Halt,
        EXHAUSTED_DETACH => OnExhausted::Detach,
        _ => return Err(PlanDecodeError::InvalidLayout("on_exhausted")),
    };
    if read_u16(header, 14)? != 0 || read_u64(header, 16)? != 0 {
        return Err(PlanDecodeError::InvalidLayout("reserved"));
    }
    let flags = *header.get(5).ok_or(truncated(bytes, at + 6))?;
    if flags & !FLAG_ESTABLISHES_SESSIONS != 0 {
        return Err(PlanDecodeError::InvalidLayout("flags"));
    }
    let executable_len = usize::from(read_u16(header, 6)?);
    let name_len = usize::from(read_u16(header, 8)?);
    let slot_count = read_u16(header, 10)?;
    let after_count = read_u16(header, 12)?;
    let memory_pages = read_u64(header, 24)?;
    if memory_pages == 0 {
        return Err(PlanDecodeError::InvalidLayout("memory_pages"));
    }
    let startup_timeout = Timeout::from_ns(read_u64(header, 32)?);
    if startup_timeout == Timeout::POLL || startup_timeout == Timeout::INFINITE {
        return Err(PlanDecodeError::InvalidLayout("startup_timeout"));
    }

    let cursor = at + COMPONENT_HEADER_SIZE;
    let (executable, cursor) = parse_str(bytes, cursor, executable_len, "executable")?;
    let (name, mut cursor) = parse_str(bytes, cursor, name_len, "name")?;

    let slots_at = cursor;
    for _ in 0..slot_count {
        cursor = parse_slot(bytes, cursor)?.end;
    }
    let slots = bytes
        .get(slots_at..cursor)
        .ok_or(truncated(bytes, cursor))?;

    let after_at = cursor;
    cursor = parse_names(bytes, cursor, after_count, "edge")?;
    let after = bytes
        .get(after_at..cursor)
        .ok_or(truncated(bytes, cursor))?;

    Ok(ParsedComponent {
        component: PlanComponent {
            executable,
            name,
            slots,
            slot_count,
            after,
            after_count,
            memory_pages,
            startup_timeout,
            launches,
            on_exhausted,
            establishes_sessions: flags & FLAG_ESTABLISHES_SESSIONS != 0,
        },
        end: cursor,
    })
}

fn parse_slot(bytes: &[u8], at: usize) -> Result<ParsedSlot<'_>, PlanDecodeError> {
    let header = bytes
        .get(at..at + SLOT_HEADER_SIZE)
        .ok_or(truncated(bytes, at + SLOT_HEADER_SIZE))?;
    let Some((kind, rest)) = header.split_first() else {
        return Err(truncated(bytes, at + SLOT_HEADER_SIZE));
    };
    let kind = *kind;
    if rest.first() != Some(&0) || read_u16(header, 10)? != 0 {
        return Err(PlanDecodeError::InvalidLayout("reserved"));
    }
    let provider_len = usize::from(read_u16(header, 2)?);
    let instance = read_u16(header, 4)?;
    let resource = read_u16(header, 6)?;
    let selector_count = read_u16(header, 8)?;

    let cursor = at + SLOT_HEADER_SIZE;
    let device_kind = matches!(kind, SLOT_KIND_IRQ | SLOT_KIND_MMIO);
    let provider_kind = matches!(
        kind,
        SLOT_KIND_SERVICE | SLOT_KIND_DIRECTORY | SLOT_KIND_CONTROL
    );
    if !device_kind && !provider_kind && kind != SLOT_KIND_USERLAND && kind != SLOT_KIND_SUPERVISOR
    {
        return Err(PlanDecodeError::InvalidLayout("slot_kind"));
    }
    if !device_kind && (instance != 0 || resource != 0) {
        return Err(PlanDecodeError::InvalidLayout("slot_device"));
    }
    if device_kind && selector_count == 0 {
        return Err(PlanDecodeError::InvalidLayout("slot_selector"));
    }
    // Строки совместимости несёт только устройство.
    if !device_kind && selector_count != 0 {
        return Err(PlanDecodeError::InvalidLayout("slot_strings"));
    }
    // Имя экземпляра-провайдера несут только слоты, которые он наполняет.
    if !provider_kind && provider_len != 0 {
        return Err(PlanDecodeError::InvalidLayout("slot_provider"));
    }

    let (source, end) = match kind {
        SLOT_KIND_USERLAND => (SlotSource::Userland, cursor),
        SLOT_KIND_SERVICE | SLOT_KIND_DIRECTORY | SLOT_KIND_CONTROL => {
            let (provider, end) = parse_str(bytes, cursor, provider_len, "slot_provider")?;
            let source = match kind {
                SLOT_KIND_SERVICE => SlotSource::Service { provider },
                SLOT_KIND_DIRECTORY => SlotSource::Directory { provider },
                _ => SlotSource::Control { provider },
            };
            (source, end)
        }
        SLOT_KIND_SUPERVISOR => (SlotSource::Supervisor, cursor),
        SLOT_KIND_IRQ | SLOT_KIND_MMIO => {
            let end = parse_names(bytes, cursor, selector_count, "slot_selector")?;
            let device = PlanDevice {
                selectors: bytes.get(cursor..end).ok_or(truncated(bytes, end))?,
                selector_count,
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
    use crate::{ComponentSpec, DeviceSpec, PlanEncodeError, SlotSpec, encode_plan};

    const RTC_SELECTORS: &[&str] = &["arm,pl031", "arm,primecell"];

    const IMAGE_FS_NAME: &str = "imagefs";
    const ROUTER_NAME: &str = "router-a";

    fn router() -> ComponentSpec<'static> {
        ComponentSpec {
            executable: "/boot/namespace-router",
            name: ROUTER_NAME,
            slots: &[],
            after: &[],
            memory_pages: 2_048,
            startup_timeout: Timeout::from_millis(2_000),
            launches: 3,
            on_exhausted: OnExhausted::Halt,
            establishes_sessions: false,
        }
    }

    fn image_filesystem() -> ComponentSpec<'static> {
        ComponentSpec {
            executable: "/boot/filesystem-imagefs",
            name: IMAGE_FS_NAME,
            slots: &[SlotSpec::Userland],
            after: &[],
            memory_pages: 2_048,
            startup_timeout: Timeout::from_millis(2_000),
            launches: 3,
            on_exhausted: OnExhausted::Halt,
            establishes_sessions: true,
        }
    }

    fn rtc() -> ComponentSpec<'static> {
        ComponentSpec {
            executable: "/boot/driver-pl031",
            name: "clock",
            slots: &[
                SlotSpec::Irq(DeviceSpec {
                    compatible: RTC_SELECTORS,
                    instance: 0,
                    resource: 0,
                }),
                SlotSpec::Mmio(DeviceSpec {
                    compatible: RTC_SELECTORS,
                    instance: 0,
                    resource: 1,
                }),
                SlotSpec::Service {
                    provider: IMAGE_FS_NAME,
                },
            ],
            after: &[IMAGE_FS_NAME],
            memory_pages: 64,
            startup_timeout: Timeout::from_millis(1_000),
            launches: 1,
            on_exhausted: OnExhausted::Detach,
            establishes_sessions: false,
        }
    }

    fn plan_bytes() -> Vec<u8> {
        encode_plan(&[image_filesystem(), rtc()]).expect("encode plan")
    }

    #[test]
    fn round_trip_exposes_every_component_and_its_params() {
        let bytes = plan_bytes();
        let plan = decode_plan(&bytes).expect("decode plan");
        assert_eq!(plan.len(), 2);

        let filesystem = plan.component(0).expect("first component");
        assert_eq!(filesystem.executable(), "/boot/filesystem-imagefs");
        assert_eq!(filesystem.name(), "imagefs");
        assert_eq!(filesystem.memory_pages(), 2_048);
        assert_eq!(filesystem.startup_timeout(), Timeout::from_millis(2_000));
        assert_eq!(filesystem.launches(), 3);
        assert_eq!(filesystem.on_exhausted(), OnExhausted::Halt);
        assert!(filesystem.establishes_sessions());
        assert_eq!(
            filesystem.slots().collect::<Vec<_>>(),
            vec![SlotSource::Userland]
        );
        assert_eq!(filesystem.after().count(), 0);

        let rtc = plan.component(1).expect("second component");
        assert_eq!(rtc.name(), "clock");
        assert_eq!(rtc.on_exhausted(), OnExhausted::Detach);
        // Сессии драйвера состояния не несут: их минтит supervisor.
        assert!(!rtc.establishes_sessions());
        assert_eq!(rtc.slot_count(), 3);
        let slots: Vec<SlotSource<'_>> = rtc.slots().collect();
        let SlotSource::Irq(line) = slots[0] else {
            panic!("first slot is an interrupt line")
        };
        assert_eq!(line.compatible().collect::<Vec<_>>(), RTC_SELECTORS);
        assert_eq!(line.resource, 0);
        let SlotSource::Mmio(window) = slots[1] else {
            panic!("second slot is an MMIO window")
        };
        assert_eq!(window.compatible().collect::<Vec<_>>(), RTC_SELECTORS);
        assert_eq!(window.resource, 1);
        assert_eq!(
            slots[2],
            SlotSource::Service {
                provider: IMAGE_FS_NAME
            }
        );
        assert_eq!(rtc.after().collect::<Vec<_>>(), vec![IMAGE_FS_NAME]);
    }

    #[test]
    fn round_trip_carries_a_directory_slot_with_its_provider() {
        let consumer = ComponentSpec {
            slots: &[SlotSpec::Directory {
                provider: ROUTER_NAME,
            }],
            after: &[],
            ..rtc()
        };
        let bytes = encode_plan(&[router(), consumer]).expect("encode plan");

        let plan = decode_plan(&bytes).expect("decode plan");
        let consumer = plan.component(1).expect("second component");
        assert_eq!(
            consumer.slots().collect::<Vec<_>>(),
            vec![SlotSource::Directory {
                provider: ROUTER_NAME
            }]
        );
    }

    #[test]
    fn round_trip_carries_a_control_slot_with_its_provider() {
        let consumer = ComponentSpec {
            slots: &[SlotSpec::Control {
                provider: ROUTER_NAME,
            }],
            after: &[],
            ..rtc()
        };
        let bytes = encode_plan(&[router(), consumer]).expect("encode plan");

        let plan = decode_plan(&bytes).expect("decode plan");
        let consumer = plan.component(1).expect("second component");
        assert_eq!(
            consumer.slots().collect::<Vec<_>>(),
            vec![SlotSource::Control {
                provider: ROUTER_NAME
            }]
        );
    }

    #[test]
    fn round_trip_names_a_provider_outside_the_record_of_its_consumer() {
        // Имя работает сквозь ступени: провайдер лежит вне этой записи, и слот
        // называет его именем экземпляра.
        let consumer = ComponentSpec {
            slots: &[SlotSpec::Service {
                provider: "stage-zero-ramfs",
            }],
            after: &["stage-zero-router"],
            ..rtc()
        };
        let bytes = encode_plan(&[consumer]).expect("encode plan");

        let plan = decode_plan(&bytes).expect("decode plan");
        let consumer = plan.component(0).expect("only component");
        assert_eq!(
            consumer.slots().collect::<Vec<_>>(),
            vec![SlotSource::Service {
                provider: "stage-zero-ramfs"
            }]
        );
        assert_eq!(
            consumer.after().collect::<Vec<_>>(),
            vec!["stage-zero-router"]
        );
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
        let field = |offset: usize, value: u64| {
            let mut bytes = plan_bytes();
            let at = PLAN_HEADER_SIZE + offset;
            bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
            decode_plan(&bytes).expect_err("component must be runnable")
        };

        assert_eq!(field(24, 0), PlanDecodeError::InvalidLayout("memory_pages"));
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
            PlanDecodeError::InvalidLayout("launches")
        );
    }

    #[test]
    fn decode_rejects_an_outcome_outside_its_values() {
        let mut bytes = plan_bytes();
        bytes[PLAN_HEADER_SIZE + 4] = 2;
        assert_eq!(
            decode_plan(&bytes).expect_err("exhausted outcome is a closed set"),
            PlanDecodeError::InvalidLayout("on_exhausted")
        );
    }

    #[test]
    fn decode_rejects_a_reserved_field_in_use() {
        for offset in [14, 16] {
            let mut bytes = plan_bytes();
            bytes[PLAN_HEADER_SIZE + offset] = 1;
            assert_eq!(
                decode_plan(&bytes).expect_err("reserved bytes are zero"),
                PlanDecodeError::InvalidLayout("reserved")
            );
        }
    }

    #[test]
    fn decode_rejects_a_flag_outside_the_declared_set() {
        let mut bytes = plan_bytes();
        bytes[PLAN_HEADER_SIZE + 5] = 0b10;
        assert_eq!(
            decode_plan(&bytes).expect_err("component flags are a closed set"),
            PlanDecodeError::InvalidLayout("flags")
        );
    }

    #[test]
    fn decode_rejects_a_string_that_is_not_utf8() {
        let mut bytes = plan_bytes();
        let executable_at = PLAN_HEADER_SIZE + COMPONENT_HEADER_SIZE;
        bytes[executable_at] = 0xFF;
        assert_eq!(
            decode_plan(&bytes).expect_err("strings are UTF-8"),
            PlanDecodeError::InvalidLayout("executable")
        );
    }

    #[test]
    fn decode_rejects_a_slot_of_an_unknown_kind() {
        let bytes = encode_plan(&[rtc()]).expect("encode plan");
        let slot_at = first_slot_offset(&rtc());
        let mut broken = bytes;
        broken[slot_at] = 9;
        assert_eq!(
            decode_plan(&broken).expect_err("slot kinds are closed"),
            PlanDecodeError::InvalidLayout("slot_kind")
        );
    }

    #[test]
    fn decode_rejects_a_device_slot_without_selectors() {
        let bytes = encode_plan(&[rtc()]).expect("encode plan");
        let count_at = first_slot_offset(&rtc()) + 8;
        let mut broken = bytes;
        broken[count_at..count_at + 2].copy_from_slice(&0u16.to_le_bytes());
        assert_eq!(
            decode_plan(&broken).expect_err("a device slot names its compatibles"),
            PlanDecodeError::InvalidLayout("slot_selector")
        );
    }

    #[test]
    fn decode_rejects_a_provider_slot_without_a_name() {
        let component = ComponentSpec {
            slots: &[SlotSpec::Directory {
                provider: ROUTER_NAME,
            }],
            ..router()
        };
        let bytes = encode_plan(&[component]).expect("encode plan");
        let provider_at = first_slot_offset(&component) + 2;
        let mut broken = bytes;
        broken[provider_at..provider_at + 2].copy_from_slice(&0u16.to_le_bytes());

        assert_eq!(
            decode_plan(&broken).expect_err("provider slot names its instance"),
            PlanDecodeError::InvalidLayout("slot_provider")
        );
    }

    #[test]
    fn decode_rejects_a_provider_name_on_a_slot_without_one() {
        let component = ComponentSpec {
            slots: &[SlotSpec::Userland],
            ..router()
        };
        let bytes = encode_plan(&[component]).expect("encode plan");
        let provider_at = first_slot_offset(&component) + 2;
        let mut broken = bytes;
        broken[provider_at..provider_at + 2].copy_from_slice(&4u16.to_le_bytes());

        assert_eq!(
            decode_plan(&broken).expect_err("only a provider slot names an instance"),
            PlanDecodeError::InvalidLayout("slot_provider")
        );
    }

    fn first_slot_offset(component: &ComponentSpec<'_>) -> usize {
        // Слоты идут сразу за строками записи компонента.
        PLAN_HEADER_SIZE + COMPONENT_HEADER_SIZE + component.executable.len() + component.name.len()
    }

    #[test]
    fn decode_rejects_an_edge_without_a_name() {
        let bytes = plan_bytes();
        let edge_at = bytes.len() - NAME_LEN_SIZE - IMAGE_FS_NAME.len();
        let mut broken = bytes;
        broken[edge_at..edge_at + NAME_LEN_SIZE].copy_from_slice(&0u16.to_le_bytes());
        assert_eq!(
            decode_plan(&broken).expect_err("edges name instances"),
            PlanDecodeError::InvalidLayout("edge")
        );
    }

    #[test]
    fn encode_rejects_a_composition_that_cannot_run() {
        let zero_memory = ComponentSpec {
            memory_pages: 0,
            ..router()
        };
        assert_eq!(
            encode_plan(&[zero_memory]).expect_err("memory limit must be non-zero"),
            PlanEncodeError::ZeroMemory
        );

        let no_launches = ComponentSpec {
            launches: 0,
            ..router()
        };
        assert_eq!(
            encode_plan(&[no_launches]).expect_err("component must launch once"),
            PlanEncodeError::ZeroLaunches
        );

        let unbounded = ComponentSpec {
            startup_timeout: Timeout::INFINITE,
            ..router()
        };
        assert_eq!(
            encode_plan(&[unbounded]).expect_err("startup timeout must be finite"),
            PlanEncodeError::UnboundedStartupTimeout
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

        let nameless_provider = ComponentSpec {
            slots: &[SlotSpec::Service { provider: "" }],
            ..router()
        };
        assert_eq!(
            encode_plan(&[nameless_provider]).expect_err("provider names an instance"),
            PlanEncodeError::EmptyString
        );

        let nameless_directory = ComponentSpec {
            slots: &[SlotSpec::Directory { provider: "" }],
            ..router()
        };
        assert_eq!(
            encode_plan(&[nameless_directory]).expect_err("provider names an instance"),
            PlanEncodeError::EmptyString
        );

        let nameless_control = ComponentSpec {
            slots: &[SlotSpec::Control { provider: "" }],
            ..router()
        };
        assert_eq!(
            encode_plan(&[nameless_control]).expect_err("provider names an instance"),
            PlanEncodeError::EmptyString
        );

        let nameless_edge = ComponentSpec {
            after: &[""],
            ..router()
        };
        assert_eq!(
            encode_plan(&[nameless_edge]).expect_err("edges name instances"),
            PlanEncodeError::EmptyString
        );
    }
}
