//! Разбор набора настроек TLMM в записи выводов.

use alloc::vec::Vec;

use pins_abi::{PinsError, pin_names, pin_number};
use platform_abi::Node;

use crate::registers::{PinGroup, Tile};

const PINS: &str = "pins";
const FUNCTION: &str = "function";
const DRIVE_STRENGTH: &str = "drive-strength";

const BIASES: [(&str, u32); 4] = [
    ("bias-disable", 0),
    ("bias-pull-down", 1),
    ("bias-bus-hold", 2),
    ("bias-pull-up", 3),
];

const GPIO_FUNCTION: &str = "gpio";
pub(crate) const GPIO_FUNCTION_NUMBER: u32 = 0;

const DRIVE_MIN_MA: u32 = 2;
const DRIVE_MAX_MA: u32 = 16;
const DRIVE_STEP_MA: u32 = 2;

#[derive(Debug, Clone, Copy)]
pub(crate) struct Setting {
    pub(crate) pin: u16,
    pub(crate) tile: Tile,
    pub(crate) function: Option<u32>,
    pub(crate) pull: Option<u32>,
    pub(crate) drive: Option<u32>,
}

pub(crate) fn parse(
    set: &Node<'_>,
    groups: &[PinGroup],
    settings: &mut Vec<Setting>,
) -> Result<(), PinsError> {
    let mut pull = None;
    for name in set.property_names() {
        if let Some((_, value)) = BIASES.iter().find(|(bias, _)| *bias == name) {
            pull = Some(*value);
        } else if ![PINS, FUNCTION, DRIVE_STRENGTH].contains(&name) {
            return Err(PinsError::Unsupported);
        }
    }

    let function = set.string(FUNCTION).map_err(|_| PinsError::Unsupported)?;
    let drive = set
        .u32(DRIVE_STRENGTH)
        .map_err(|_| PinsError::Unsupported)?
        .map(drive_field)
        .transpose()?;

    for name in pin_names(set).ok_or(PinsError::Unsupported)? {
        let pin = pin_number(name).ok_or(PinsError::Unsupported)?;
        let group = groups.get(usize::from(pin)).ok_or(PinsError::Unsupported)?;
        let function = function
            .map(|function| function_number(group, function))
            .transpose()?;

        settings.push(Setting {
            pin,
            tile: group.tile,
            function,
            pull,
            drive,
        });
    }

    Ok(())
}

fn function_number(group: &PinGroup, function: &str) -> Result<u32, PinsError> {
    if function == GPIO_FUNCTION {
        return Ok(GPIO_FUNCTION_NUMBER);
    }

    let position = group
        .functions
        .iter()
        .position(|each| !each.is_empty() && *each == function)
        .ok_or(PinsError::Unsupported)?;
    u32::try_from(position + 1).map_err(|_| PinsError::Unsupported)
}

fn drive_field(milliamps: u32) -> Result<u32, PinsError> {
    if !(DRIVE_MIN_MA..=DRIVE_MAX_MA).contains(&milliamps)
        || !milliamps.is_multiple_of(DRIVE_STEP_MA)
    {
        return Err(PinsError::Unsupported);
    }
    Ok(milliamps / DRIVE_STEP_MA - 1)
}
