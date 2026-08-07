use syscall::Timeout;

/// Путь записи образа, из которой rootkeeper читает план загрузочной композиции.
pub const BOOT_PLAN_PATH: &str = "/boot/rootkeeper.plan";

pub const BOOT_PLAN_MAGIC: [u8; 8] = *b"RKPLAN\0\0";
pub const BOOT_PLAN_VERSION: u16 = 1;

pub(crate) const PLAN_HEADER_SIZE: usize = 16;
pub(crate) const COMPONENT_HEADER_SIZE: usize = 32;

pub(crate) const PLAN_COMPONENT_COUNT: u16 = 1;

/// Компонент загрузочной композиции.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanComponent<'a> {
    id: &'a str,
    program: &'a str,
    config: &'a str,
    budget_pages: u64,
    startup_timeout: Timeout,
    restart_limit: u32,
    critical: bool,
}

/// План загрузочной композиции, разобранный из записи образа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootPlan<'a> {
    component: PlanComponent<'a>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanDecodeError {
    InvalidMagic([u8; 8]),
    InvalidVersion(u16),
    Truncated { needed: usize, actual: usize },
    InvalidLayout(&'static str),
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
    pub const fn config(&self) -> &'a str {
        self.config
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
    /// Компонент композиции.
    #[must_use]
    pub const fn component(&self) -> PlanComponent<'a> {
        self.component
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
        if read_u16(header, 10)? != PLAN_COMPONENT_COUNT {
            return Err(PlanDecodeError::InvalidLayout("component_count"));
        }
        if read_u32(header, 12)? != 0 {
            return Err(PlanDecodeError::InvalidLayout("reserved"));
        }

        let (component, end) = parse_component(bytes, PLAN_HEADER_SIZE)?;
        if end != bytes.len() {
            return Err(PlanDecodeError::InvalidLayout("trailing_bytes"));
        }
        Ok(Self { component })
    }
}

fn parse_component(bytes: &[u8], at: usize) -> Result<(PlanComponent<'_>, usize), PlanDecodeError> {
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
    if header[5] != 0 || read_u32(header, 12)? != 0 {
        return Err(PlanDecodeError::InvalidLayout("reserved"));
    }
    let id_len = usize::from(read_u16(header, 6)?);
    let program_len = usize::from(read_u16(header, 8)?);
    let config_len = usize::from(read_u16(header, 10)?);
    let budget_pages = read_u64(header, 16)?;
    if budget_pages == 0 {
        return Err(PlanDecodeError::InvalidLayout("budget_pages"));
    }
    let startup_timeout = Timeout::from_ns(read_u64(header, 24)?);
    if startup_timeout == Timeout::POLL || startup_timeout == Timeout::INFINITE {
        return Err(PlanDecodeError::InvalidLayout("startup_timeout"));
    }

    let cursor = at + COMPONENT_HEADER_SIZE;
    let (id, cursor) = parse_str(bytes, cursor, id_len, "id")?;
    let (program, cursor) = parse_str(bytes, cursor, program_len, "program")?;
    let (config, cursor) = parse_str(bytes, cursor, config_len, "config")?;

    Ok((
        PlanComponent {
            id,
            program,
            config,
            budget_pages,
            startup_timeout,
            restart_limit,
            critical,
        },
        cursor,
    ))
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
    use super::*;
    use crate::{ComponentSpec, PlanEncodeError, encode_plan};

    const CONFIG: &str = "/boot/namespace.mounts";

    fn spec() -> ComponentSpec<'static> {
        ComponentSpec {
            id: "filesystem",
            program: "/boot/namespace-router",
            config: CONFIG,
            budget_pages: 2_048,
            startup_timeout: Timeout::from_millis(2_000),
            restart_limit: 3,
            critical: true,
        }
    }

    fn plan_bytes() -> Vec<u8> {
        encode_plan(&spec()).expect("encode plan")
    }

    #[test]
    fn round_trip_exposes_the_component_and_its_params() {
        let bytes = plan_bytes();
        let component = decode_plan(&bytes).expect("decode plan").component();

        assert_eq!(component.id(), "filesystem");
        assert_eq!(component.program(), "/boot/namespace-router");
        assert_eq!(component.budget_pages(), 2_048);
        assert_eq!(component.startup_timeout(), Timeout::from_millis(2_000));
        assert_eq!(component.restart_limit(), 3);
        assert!(component.critical());

        assert_eq!(component.config(), CONFIG);
    }

    #[test]
    fn round_trip_keeps_an_optional_component() {
        let mut spec = spec();
        spec.critical = false;
        let bytes = encode_plan(&spec).expect("encode plan");

        let component = decode_plan(&bytes).expect("decode plan").component();
        assert!(!component.critical());
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
        bytes[8..10].copy_from_slice(&2u16.to_le_bytes());
        assert_eq!(
            decode_plan(&bytes).expect_err("version must match"),
            PlanDecodeError::InvalidVersion(2)
        );
    }

    #[test]
    fn decode_rejects_a_composition_of_another_size() {
        let mut bytes = plan_bytes();
        bytes[10..12].copy_from_slice(&2u16.to_le_bytes());
        assert_eq!(
            decode_plan(&bytes).expect_err("v1 carries one component"),
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
    fn decode_rejects_bytes_past_the_component() {
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

        assert_eq!(field(16, 0), PlanDecodeError::InvalidLayout("budget_pages"));
        assert_eq!(
            field(24, Timeout::POLL.raw()),
            PlanDecodeError::InvalidLayout("startup_timeout")
        );
        assert_eq!(
            field(24, Timeout::INFINITE.raw()),
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
    fn decode_rejects_a_component_without_its_config() {
        let mut bytes = plan_bytes();
        let at = PLAN_HEADER_SIZE + 10;
        bytes[at..at + 2].copy_from_slice(&0u16.to_le_bytes());
        assert_eq!(
            decode_plan(&bytes).expect_err("component names its config record"),
            PlanDecodeError::InvalidLayout("config")
        );
    }

    #[test]
    fn encode_rejects_a_component_that_cannot_run() {
        let zero_budget = ComponentSpec {
            budget_pages: 0,
            ..spec()
        };
        assert_eq!(
            encode_plan(&zero_budget).expect_err("budget must be non-zero"),
            PlanEncodeError::ZeroBudget
        );

        let no_launches = ComponentSpec {
            restart_limit: 0,
            ..spec()
        };
        assert_eq!(
            encode_plan(&no_launches).expect_err("component must launch once"),
            PlanEncodeError::ZeroRestartLimit
        );

        let unbounded = ComponentSpec {
            startup_timeout: Timeout::INFINITE,
            ..spec()
        };
        assert_eq!(
            encode_plan(&unbounded).expect_err("startup timeout must be finite"),
            PlanEncodeError::UnboundedStartupTimeout
        );

        let nameless = ComponentSpec { id: "", ..spec() };
        assert_eq!(
            encode_plan(&nameless).expect_err("id must be non-empty"),
            PlanEncodeError::EmptyString
        );
    }
}
