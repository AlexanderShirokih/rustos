use core::fmt;
use core::str;

use fdt::devicetree::{Node, Property};
use fdt::devicetreeext::NodeExt as DtNodeExt;

pub type CompatibleList = &'static [&'static str];
pub type ProbeResult<T> = Result<T, ProbeError>;
pub type MmioAddress = usize;
pub type EndpointId = u64;

/// Запрос на маппинг MMIO региона от драйвера
#[derive(Debug, Clone, Copy)]
pub struct MmioRequest {
    pub base: MmioAddress,
    pub size: usize,
}

#[derive(Debug)]
pub enum ProbeError {
    MissingProperty(&'static str),
    Unsupported(&'static str),
    Other(&'static str),
}

impl fmt::Display for ProbeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProbeError::MissingProperty(prop) => write!(f, "missing property '{}'", prop),
            ProbeError::Unsupported(feature) => write!(f, "unsupported: {}", feature),
            ProbeError::Other(msg) => f.write_str(msg),
        }
    }
}

pub trait NodeProbeExt<'a> {
    fn require_prop(&self, name: &'static str) -> ProbeResult<Property<'a>>;
    fn is_compatible_any(&self, candidates: CompatibleList) -> bool;
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

pub struct CompatibleStrings<'a> {
    data: &'a [u8],
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
