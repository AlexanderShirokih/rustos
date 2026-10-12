//! Записи `clocks` узла устройства.

use alloc::vec::Vec;

use clocks_abi::ClocksError;
use platform_abi::{DeviceId, Node};

const CLOCKS: &str = "clocks";
const CLOCK_CELLS: &str = "#clock-cells";
const CLOCK_NAMES: &str = "clock-names";

pub(crate) struct ClockRecord<'d> {
    pub(crate) controller: DeviceId,
    pub(crate) cells: Vec<u32>,
    pub(crate) name: Option<&'d str>,
}

pub(crate) fn records<'d>(node: &Node<'d>) -> Result<Vec<ClockRecord<'d>>, ClocksError> {
    let Some(specifiers) = node.specifiers(CLOCKS, CLOCK_CELLS) else {
        return Ok(Vec::new());
    };
    let mut names = node
        .strings(CLOCK_NAMES)
        .map_err(|_| ClocksError::Unsupported)?;

    specifiers
        .map(|specifier| {
            let specifier = specifier.map_err(|_| ClocksError::Unsupported)?;
            Ok(ClockRecord {
                cells: specifier.cells().collect(),
                controller: specifier.node,
                name: names.as_mut().and_then(Iterator::next),
            })
        })
        .collect()
}
