use alloc::{format, vec::Vec};

use platform_abi::{Description, DeviceId, Node};

const DEFAULT_STATE: &str = "default";
const STATE_NAMES: &str = "pinctrl-names";

/// Узел состояния `default` узла и его контроллер.
pub struct PinState<'d> {
    /// Узел состояния.
    pub node: Node<'d>,
    /// Путь контроллера состояния: ближайшего к узлу состояния предка с `compatible`.
    pub controller: DeviceId,
}

/// Возвращает узлы состояния `default` доступного узла `device` в порядке записи состояния.
///
/// Состояние `default` - запись `pinctrl-N`, где `N` - позиция строки `default` в
/// `pinctrl-names`. Узел состояния без предка с `compatible` пропускается. Отсутствующий либо
/// недоступный узел и узел без строки `default` состояний не имеют.
///
/// Возвращает `None`, если:
/// - ссылка записи не называет узла;
/// - значение `pinctrl-names` либо записи повреждено.
pub fn default_states<'d>(
    description: &'d Description<'_>,
    device: &str,
) -> Option<Vec<PinState<'d>>> {
    let mut states = Vec::new();
    let Some(node) = DeviceId::new(device).and_then(|id| description.node(&id)) else {
        return Some(states);
    };

    let names = node.strings(STATE_NAMES).ok()?;
    let Some(index) = names.and_then(|mut names| names.position(|name| name == DEFAULT_STATE))
    else {
        return Some(states);
    };

    let Some(references) = node.references(&format!("pinctrl-{index}")) else {
        return Some(states);
    };

    for id in references {
        let id = id.ok()?;
        let node = description.find(id.as_str())?;
        let controller = description
            .ancestors(&id)
            .find(|ancestor| ancestor.node.bytes("compatible").is_some());

        if let Some(controller) = controller {
            states.push(PinState {
                node,
                controller: controller.id,
            });
        }
    }

    Some(states)
}

/// Возвращает узлы контроллеров состояния `default` узла `device` по [`default_states`],
/// каждый один раз, в порядке первого состояния контроллера.
///
/// Возвращает `None` в тех же случаях, что [`default_states`].
pub fn controllers<'d>(description: &'d Description<'_>, device: &str) -> Option<Vec<Node<'d>>> {
    let mut paths: Vec<DeviceId> = Vec::new();
    for state in default_states(description, device)? {
        if !paths.contains(&state.controller) {
            paths.push(state.controller);
        }
    }

    Some(
        paths
            .iter()
            .filter_map(|path| description.find(path.as_str()))
            .collect(),
    )
}
