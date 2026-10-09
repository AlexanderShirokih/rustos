use crate::entry_name;

/// Путь записи образа, из которой rootkeeper читает план загрузочной композиции.
pub const BOOT_PLAN_PATH: &str = "/rootkeeper/plan";

/// Имя записи с программой в каталоге компонента.
pub const COMPONENT_PROGRAM_NAME: &str = "exec";

/// Имя записи `exec` внутри каталога компонента - имя этого каталога. Любая другая
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

pub(crate) const PLAN_HEADER_SIZE: usize = 12;
pub(crate) const COMPONENT_HEADER_SIZE: usize = 16;

pub(crate) const COMPONENT_RESTARTS: usize = 0;
pub(crate) const COMPONENT_EXECUTABLE_LEN: usize = 4;
pub(crate) const COMPONENT_NAME_LEN: usize = 6;
pub(crate) const COMPONENT_MEMORY_LIMIT: usize = 8;

/// Компонент загрузочной композиции.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanComponent<'a> {
    executable: &'a str,
    name: &'a str,
    memory_limit: u64,
    restarts: u32,
}

/// План загрузочной композиции, разобранный из записи образа.
///
/// Запись 0 - загрузочная программа образа с путём и именем. У неё нет предела памяти и
/// перезапусков, и родитель её не запускает. Записи с номера 1 - компоненты композиции.
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

/// Причина отказа декодера записи (или чтении) плана.
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

    /// Предел памяти компонента в байтах. Родитель отрезает компоненту бюджет не меньше
    /// предела. У записи 0 предел нулевой, у остальных записей ненулевой.
    #[must_use]
    pub const fn memory_limit(&self) -> u64 {
        self.memory_limit
    }

    /// Число перезапусков после первого запуска. У записи 0 равно нулю.
    #[must_use]
    pub const fn restarts(&self) -> u32 {
        self.restarts
    }
}

impl<'a> BootPlan<'a> {
    /// Число записей плана вместе с записью 0.
    #[must_use]
    pub const fn len(&self) -> u16 {
        self.count
    }

    /// Разобранный план содержит запись 0 и не пуст.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Записи плана в порядке записи, начиная с записи 0.
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
            return Err(PlanDecodeError::InvalidLayout("boot_record"));
        }

        let mut cursor = PLAN_HEADER_SIZE;
        for index in 0..count {
            let parsed = parse_component(bytes, cursor)?;
            let component = parsed.component;
            if index == 0 {
                if component.memory_limit != 0 || component.restarts != 0 {
                    return Err(PlanDecodeError::InvalidLayout("boot_record"));
                }
            } else if component.memory_limit == 0 {
                return Err(PlanDecodeError::InvalidLayout("memory_limit"));
            }
            cursor = parsed.end;
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
        // Запись уже проверена декодированием плана целиком, а повторный разбор
        // читает те же байты.
        let parsed = parse_component(self.bytes, self.cursor).ok()?;
        self.remaining -= 1;
        self.cursor = parsed.end;
        Some(parsed.component)
    }
}

fn parse_component(bytes: &[u8], at: usize) -> Result<ParsedComponent<'_>, PlanDecodeError> {
    let header = bytes
        .get(at..at + COMPONENT_HEADER_SIZE)
        .ok_or(truncated(bytes, at + COMPONENT_HEADER_SIZE))?;
    let restarts = read_u32(header, COMPONENT_RESTARTS)?;
    let executable_len = usize::from(read_u16(header, COMPONENT_EXECUTABLE_LEN)?);
    let name_len = usize::from(read_u16(header, COMPONENT_NAME_LEN)?);
    let memory_limit = read_u64(header, COMPONENT_MEMORY_LIMIT)?;

    let cursor = at + COMPONENT_HEADER_SIZE;
    let (executable, cursor) = parse_str(bytes, cursor, executable_len, "executable")?;
    let (name, cursor) = parse_str(bytes, cursor, name_len, "name")?;

    let component = PlanComponent {
        executable,
        name,
        memory_limit,
        restarts,
    };

    Ok(ParsedComponent {
        component,
        end: cursor,
    })
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
    use alloc::vec::Vec;

    use super::*;
    use crate::{ComponentSpec, PlanEncodeError, encode_plan};

    fn boot() -> ComponentSpec<'static> {
        ComponentSpec {
            executable: "/rootkeeper",
            name: "rootkeeper",
            memory_limit: 0,
            restarts: 0,
        }
    }
    fn router() -> ComponentSpec<'static> {
        ComponentSpec {
            executable: "/mounts",
            name: "router-a",
            memory_limit: 8 * 1024 * 1024,
            restarts: 2,
        }
    }
    fn plan_bytes() -> Vec<u8> {
        encode_plan(&[boot(), router()]).expect("encode plan")
    }
    fn after_boot() -> usize {
        encode_plan(&[boot()]).expect("encode plan").len()
    }
    fn broken(bytes: &[u8], at: usize, field: &[u8]) -> Option<PlanDecodeError> {
        let mut broken = bytes.to_vec();
        broken[at..at + field.len()].copy_from_slice(field);
        decode_plan(&broken).err()
    }

    #[test]
    fn encode_rejects_an_empty_name() {
        assert_eq!(
            encode_plan(&[
                boot(),
                ComponentSpec {
                    name: "",
                    ..router()
                }
            ]),
            Err(PlanEncodeError::EmptyString)
        );
    }

    #[test]
    fn decode_rejects_a_boot_record_with_runtime_properties() {
        let bytes = plan_bytes();
        for (offset, value) in [
            (COMPONENT_RESTARTS, 1u32.to_le_bytes().to_vec()),
            (COMPONENT_MEMORY_LIMIT, 1u64.to_le_bytes().to_vec()),
        ] {
            assert_eq!(
                broken(&bytes, PLAN_HEADER_SIZE + offset, &value),
                Some(PlanDecodeError::InvalidLayout("boot_record"))
            );
        }
    }

    #[test]
    fn encode_rejects_zero_memory_for_an_ordinary_component() {
        assert_eq!(
            encode_plan(&[
                boot(),
                ComponentSpec {
                    memory_limit: 0,
                    ..router()
                }
            ]),
            Err(PlanEncodeError::ZeroMemory)
        );
    }

    #[test]
    fn a_plan_of_the_boot_record_alone_decodes() {
        let bytes = encode_plan(&[boot()]).expect("encode plan");

        let plan = decode_plan(&bytes).expect("decode plan");
        assert_eq!(plan.len(), 1);
    }

    #[test]
    fn a_memory_limit_off_the_page_boundary_survives_the_round_trip() {
        let component = ComponentSpec {
            memory_limit: 5_000,
            ..router()
        };
        let bytes = encode_plan(&[boot(), component]).expect("encode plan");
        let plan = decode_plan(&bytes).expect("decode plan");

        assert_eq!(plan.component(1).expect("component").memory_limit(), 5_000);
    }

    #[test]
    fn round_trip_keeps_two_instances_of_one_program_apart() {
        let second = ComponentSpec {
            name: "router-b",
            ..router()
        };
        let bytes = encode_plan(&[boot(), router(), second]).expect("encode plan");

        let plan = decode_plan(&bytes).expect("decode plan");
        let first = plan.component(1).expect("first instance");
        let second = plan.component(2).expect("second instance");
        assert_eq!(first.executable(), second.executable());
        assert_eq!(first.name(), "router-a");
        assert_eq!(second.name(), "router-b");
    }

    #[test]
    fn a_program_record_is_named_by_the_directory_that_holds_it() {
        assert_eq!(component_name("/boot/mounts/exec"), "mounts");
        assert_eq!(component_name("/boot/mounts/table"), "table");
        assert_eq!(component_name("/boot/rootkeeper"), "rootkeeper");
        assert_eq!(component_name("/exec"), "exec");
        assert_eq!(component_name("exec"), "exec");
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
    fn decode_rejects_a_plan_without_records() {
        let mut bytes = plan_bytes();
        bytes[10..12].copy_from_slice(&0u16.to_le_bytes());
        assert_eq!(
            decode_plan(&bytes).expect_err("plan carries the boot record"),
            PlanDecodeError::InvalidLayout("boot_record")
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
        let at = after_boot() + COMPONENT_MEMORY_LIMIT;
        assert_eq!(
            broken(&bytes, at, &0u64.to_le_bytes()),
            Some(PlanDecodeError::InvalidLayout("memory_limit"))
        );
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
}
