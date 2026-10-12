//! Разбор наборов состояния `default` узла под контроллерами GPIO PMIC в записи выводов.

use alloc::vec::Vec;

use pins_abi::{Claims, Pin, PinsError, check_state, default_states, pin_names, pin_number, sets};
use platform_abi::{Description, DeviceId, Node};

use crate::{
    controller::{COMPATIBLE, ControllerNode},
    gpio::MODE_INPUT,
};

const PINS: &str = "pins";
const FUNCTION: &str = "function";
const INPUT_ENABLE: &str = "input-enable";
const DRIVE_STRENGTH: &str = "qcom,drive-strength";

const BIASES: [(&str, u8); 3] = [
    ("bias-pull-up", 0),
    ("bias-pull-down", 4),
    ("bias-disable", 5),
];

const NORMAL_FUNCTION: &str = "normal";
const NORMAL_FUNCTION_NUMBER: u8 = 0;

const DRIVE_FIELDS: [u8; 4] = [0, 3, 2, 1];

/// Запись вывода набора с его блоком и полями, которые задаёт набор.
#[derive(Clone, PartialEq, Eq)]
pub struct Setting {
    /// Путь контроллера вывода.
    pub controller: DeviceId,
    /// Номер вывода от 1.
    pub pin: u16,
    /// Блок вывода.
    pub block: u8,
    /// Режим вывода.
    pub mode: Option<u8>,
    /// Номер функции вывода.
    pub function: Option<u8>,
    /// Поле подтяжки.
    pub pull: Option<u8>,
    /// Поле тока.
    pub drive: Option<u8>,
}

/// Проверенные записи состояния `default` узла под контроллерами GPIO PMIC.
pub struct Plan {
    device: DeviceId,
    controllers: Vec<ControllerNode>,
    settings: Vec<Setting>,
    pins: Vec<Pin>,
}

impl Plan {
    /// Контроллеры наборов в порядке состояния, каждый один раз.
    pub fn controllers(&self) -> &[ControllerNode] {
        &self.controllers
    }

    /// Записи выводов контроллера `controller` в порядке наборов.
    pub fn settings(&self, controller: &DeviceId) -> Vec<Setting> {
        self.settings
            .iter()
            .filter(|setting| setting.controller == *controller)
            .cloned()
            .collect()
    }

    /// Занимает в `claims` узел и выводы состояния сессией со значком `badge`.
    pub fn claim(self, claims: &mut Claims, badge: u64) {
        claims.claim(badge, self.device, self.pins);
    }
}

/// Проверяет состояние `default` узла с путём `device` и возвращает записи его наборов под
/// контроллерами, совместимыми с `qcom,spmi-gpio`. Наборы других контроллеров пропускаются.
/// Регистры не читаются и не пишутся.
///
/// Отказы в порядке проверки:
/// - [`PinsError::NoDevice`] - узла нет в описании либо он недоступен;
/// - [`PinsError::Busy`] - узел занят сессией в `claims`;
/// - [`PinsError::Unsupported`] - ссылка записи состояния не называет узла либо запись
///   повреждена;
/// - [`PinsError::NoDevice`] - состояние не содержит наборов контроллеров GPIO PMIC;
/// - [`PinsError::Unsupported`] - контроллер вне таблицы числа выводов, вывод вне контроллера,
///   функция кроме `normal`, ток больше 3, свойство набора вне известных, свойство узла
///   состояния с потомками-наборами;
/// - [`PinsError::Busy`] - вывод занят сессией в `claims`.
pub fn plan(
    description: &Description<'_>,
    device: &DeviceId,
    claims: &Claims,
) -> Result<Plan, PinsError> {
    description.node(device).ok_or(PinsError::NoDevice)?;
    if claims.holds_device(device) {
        return Err(PinsError::Busy);
    }

    let states: Vec<_> = default_states(description, device.as_str())
        .ok_or(PinsError::Unsupported)?
        .into_iter()
        .filter(|state| served(description, &state.controller))
        .collect();
    if states.iter().all(|state| sets(state.node).is_empty()) {
        return Err(PinsError::NoDevice);
    }

    let mut controllers: Vec<ControllerNode> = Vec::new();
    let mut settings = Vec::new();
    for state in &states {
        check_state(&state.node)?;
        let controller =
            ControllerNode::parse(description, &state.controller).ok_or(PinsError::Unsupported)?;
        for set in sets(state.node) {
            parse(&set, &controller, &mut settings)?;
        }
        if !controllers.contains(&controller) {
            controllers.push(controller);
        }
    }

    let pins: Vec<_> = settings
        .iter()
        .map(|setting| Pin {
            controller: setting.controller.clone(),
            number: setting.pin,
        })
        .collect();
    if claims.holds_any(&pins) {
        return Err(PinsError::Busy);
    }

    Ok(Plan {
        device: device.clone(),
        controllers,
        settings,
        pins,
    })
}

pub(crate) fn served(description: &Description<'_>, id: &DeviceId) -> bool {
    description
        .find(id.as_str())
        .and_then(|node| node.strings("compatible").ok().flatten())
        .is_some_and(|mut compatible| compatible.any(|name| name == COMPATIBLE))
}

fn parse(
    set: &Node<'_>,
    controller: &ControllerNode,
    settings: &mut Vec<Setting>,
) -> Result<(), PinsError> {
    let mut pull = None;
    let mut mode = None;
    for name in set.property_names() {
        if let Some((_, value)) = BIASES.iter().find(|(bias, _)| *bias == name) {
            pull = Some(*value);
        } else if name == INPUT_ENABLE {
            mode = Some(MODE_INPUT);
        } else if ![PINS, FUNCTION, DRIVE_STRENGTH].contains(&name) {
            return Err(PinsError::Unsupported);
        }
    }

    let function = set
        .string(FUNCTION)
        .map_err(|_| PinsError::Unsupported)?
        .map(|function| {
            (function == NORMAL_FUNCTION)
                .then_some(NORMAL_FUNCTION_NUMBER)
                .ok_or(PinsError::Unsupported)
        })
        .transpose()?;
    let drive = set
        .u32(DRIVE_STRENGTH)
        .map_err(|_| PinsError::Unsupported)?
        .map(|strength| {
            usize::try_from(strength)
                .ok()
                .and_then(|strength| DRIVE_FIELDS.get(strength).copied())
                .ok_or(PinsError::Unsupported)
        })
        .transpose()?;

    for name in pin_names(set).ok_or(PinsError::Unsupported)? {
        let pin = pin_number(name).ok_or(PinsError::Unsupported)?;
        let block = controller.block(pin).ok_or(PinsError::Unsupported)?;

        settings.push(Setting {
            controller: controller.id.clone(),
            pin,
            block,
            mode,
            function,
            pull,
            drive,
        });
    }

    Ok(())
}
