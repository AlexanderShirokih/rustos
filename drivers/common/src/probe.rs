//! DeviceTree-специфичные типы и трейты для пробирования устройств.

use core::str;

use fdt::devicetree::{Node, Property};
use fdt::devicetreeext::NodeExt as DtNodeExt;
use foundation::{ProbeError, ProbeResult};

/// Список строк совместимости из DeviceTree.
pub type CompatibleList = &'static [&'static str];

/// Расширение для узлов DeviceTree при пробировании.
pub trait NodeProbeExt<'a> {
    /// Возвращает свойство или ошибку, если оно отсутствует.
    fn require_prop(&self, name: &'static str) -> ProbeResult<Property<'a>>;

    /// Проверяет совместимость узла с любой из строк.
    fn is_compatible_any(&self, candidates: CompatibleList) -> bool;

    /// Возвращает итератор по строкам совместимости.
    fn compatible_strings(&self) -> CompatibleStrings<'a>;
}

impl<'a> NodeProbeExt<'a> for Node<'a> {
    fn require_prop(&self, name: &'static str) -> ProbeResult<Property<'a>> {
        self.prop(name).ok_or(ProbeError::MissingProperty(name))
    }

    fn is_compatible_any(&self, candidates: CompatibleList) -> bool {
        candidates.iter().any(|c| self.is_compatible(c))
    }

    fn compatible_strings(&self) -> CompatibleStrings<'a> {
        self.prop("compatible")
            .map(|prop| CompatibleStrings::new(prop.value()))
            .unwrap_or_else(CompatibleStrings::empty)
    }
}

/// Итератор по строкам совместимости из свойства `compatible`.
pub struct CompatibleStrings<'a> {
    /// Сырые данные свойства.
    data: &'a [u8],
    /// Текущая позиция в данных.
    cursor: usize,
}

impl<'a> CompatibleStrings<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, cursor: 0 }
    }

    pub fn empty() -> Self {
        Self {
            data: &[],
            cursor: 0,
        }
    }
}

impl<'a> Iterator for CompatibleStrings<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        while self.cursor < self.data.len() {
            let start = self.cursor;
            let slice = &self.data[start..];
            let end_offset = slice.iter().position(|&b| b == 0).unwrap_or(slice.len());
            self.cursor += end_offset + 1;

            if end_offset == 0 {
                continue;
            }

            let candidate = &slice[..end_offset];
            if let Ok(value) = str::from_utf8(candidate) {
                return Some(value);
            }
        }

        None
    }
}
