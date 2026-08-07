use crate::platform::DeviceInterrupt;

const CELL_SIZE: usize = size_of::<u32>();

/// Ячейки одного interrupt-specifier устройства: сырые big-endian слова в
/// порядке описания.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InterruptSpecifier<'a> {
    cells: &'a [u8],
}

/// Перевод ячеек specifier конкретного контроллера в линию с триггером.
pub type SpecifierDecoder = fn(InterruptSpecifier<'_>) -> Option<DeviceInterrupt>;

/// Биндинг контроллера прерываний: совместимости его узла и разбор его
/// specifier.
#[derive(Debug, Clone, Copy)]
pub struct ControllerBinding {
    compatible: &'static [&'static str],
    decode: SpecifierDecoder,
}

/// Реестр биндингов, ключ которого - строка совместимости узла
/// `interrupt-parent`.
#[derive(Debug, Clone, Copy)]
pub struct ControllerBindings {
    entries: &'static [ControllerBinding],
}

impl<'a> InterruptSpecifier<'a> {
    /// Берёт `cells` big-endian слов подряд.
    #[must_use]
    pub const fn new(cells: &'a [u8]) -> Self {
        Self { cells }
    }

    /// Число ячеек specifier.
    #[must_use]
    pub const fn cell_count(&self) -> usize {
        self.cells.len() / CELL_SIZE
    }

    /// Ячейка `index` в порядке описания.
    #[must_use]
    pub fn cell(&self, index: usize) -> Option<u32> {
        let at = index.checked_mul(CELL_SIZE)?;
        let bytes = self.cells.get(at..at.checked_add(CELL_SIZE)?)?;
        Some(u32::from_be_bytes(bytes.try_into().ok()?))
    }
}

impl ControllerBinding {
    /// Объявляет биндинг контроллера, узел которого несёт одну из
    /// совместимостей `compatible`.
    #[must_use]
    pub const fn new(compatible: &'static [&'static str], decode: SpecifierDecoder) -> Self {
        Self { compatible, decode }
    }

    /// Переводит specifier контроллера в линию с триггером.
    #[must_use]
    pub fn decode(&self, specifier: InterruptSpecifier<'_>) -> Option<DeviceInterrupt> {
        (self.decode)(specifier)
    }
}

impl ControllerBindings {
    /// Собирает реестр из записей платформы.
    #[must_use]
    pub const fn new(entries: &'static [ControllerBinding]) -> Self {
        Self { entries }
    }

    /// Биндинг контроллера, объявившего совместимость `compatible`.
    #[must_use]
    pub fn lookup(&self, compatible: &str) -> Option<&ControllerBinding> {
        self.entries
            .iter()
            .find(|entry| entry.compatible.contains(&compatible))
    }
}

#[cfg(test)]
mod tests {
    use syscall::IrqTrigger;

    use super::*;
    use crate::platform::{ControllerId, InterruptLine};

    const FIRST_COMPATIBLE: &[&str] = &["test,controller", "test,controller-v2"];
    const SECOND_COMPATIBLE: &[&str] = &["test,other-controller"];

    const BINDINGS: ControllerBindings = ControllerBindings::new(&[
        ControllerBinding::new(FIRST_COMPATIBLE, decode_first),
        ControllerBinding::new(SECOND_COMPATIBLE, decode_second),
    ]);

    fn decode_first(specifier: InterruptSpecifier<'_>) -> Option<DeviceInterrupt> {
        Some(DeviceInterrupt {
            line: InterruptLine::new(
                ControllerId::PLATFORM,
                u16::try_from(specifier.cell(0)?).ok()?,
            ),
            trigger: IrqTrigger::Level,
        })
    }

    fn decode_second(_specifier: InterruptSpecifier<'_>) -> Option<DeviceInterrupt> {
        None
    }

    fn cells(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|cell| cell.to_be_bytes()).collect()
    }

    #[test]
    fn a_specifier_reads_its_cells_in_description_order() {
        let raw = cells(&[7, 9]);
        let specifier = InterruptSpecifier::new(&raw);

        assert_eq!(specifier.cell_count(), 2);
        assert_eq!(specifier.cell(0), Some(7));
        assert_eq!(specifier.cell(1), Some(9));
        assert_eq!(specifier.cell(2), None);
    }

    #[test]
    fn every_compatible_of_an_entry_leads_to_it() {
        let raw = cells(&[11]);
        for compatible in FIRST_COMPATIBLE {
            let binding = BINDINGS.lookup(compatible).expect("entry is registered");
            assert_eq!(
                binding
                    .decode(InterruptSpecifier::new(&raw))
                    .map(|interrupt| interrupt.line.number()),
                Some(11)
            );
        }
    }

    #[test]
    fn the_registry_picks_the_entry_of_its_compatible() {
        let raw = cells(&[11]);
        let binding = BINDINGS
            .lookup("test,other-controller")
            .expect("entry is registered");

        assert_eq!(binding.decode(InterruptSpecifier::new(&raw)), None);
    }

    #[test]
    fn a_controller_outside_the_registry_has_no_binding() {
        assert!(BINDINGS.lookup("test,absent-controller").is_none());
    }
}
