use fdt::{
    devicetree::{self, DeviceTree, TreeWalk},
    devicetreeext::{BUS_PATH_MAX_NODES, NodeExt, PropExt, StringList, available_path, node_path},
};
use syscall::kind;

use crate::{device::DeviceId, error::DescriptionError};

ipc::wire_value! {
    /// Описание платформы: регион только для чтения и смещение начала описания в нём.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct DescriptionRegion<M = ipc::wire::Cap<kind::MemoryRegion>> {
        /// Регион, который целиком содержит описание.
        pub region: M,
        /// Смещение начала описания от начала региона.
        pub offset: u64,
    }
}

/// Вид описания платформы поверх байт региона [`DescriptionRegion`].
pub struct Description<'a> {
    tree: DeviceTree<'a>,
}

/// Узел описания.
#[derive(Clone, Copy)]
pub struct Node<'a> {
    node: devicetree::Node<'a>,
}

/// Устройство из перечисления [`Description::devices`].
pub struct Device<'a> {
    id: DeviceId,
    compatible: StringList<'a>,
}

/// Итератор устройств, который строит [`Description::devices`]. Узел, путь которого длиннее
/// [`DEVICE_ID_MAX`](crate::DEVICE_ID_MAX) байт либо [`BUS_PATH_MAX_NODES`] узлов, даёт
/// [`DescriptionError::PathTooLong`], а узел с повреждённым списком совместимостей -
/// [`DescriptionError::InvalidValue`].
pub struct Devices<'a> {
    walk: TreeWalk<'a>,
    path: DeviceId,
    path_depth: usize,
    hidden_below: Option<usize>,
}

impl<'a> Description<'a> {
    /// Разбирает описание, которое начинается со смещения `offset` байт региона `region`.
    /// Описание, которое выходит за регион либо не разбирается, даёт
    /// [`DescriptionError::Malformed`].
    pub fn new(region: &'a [u8], offset: u64) -> Result<Self, DescriptionError> {
        let blob = usize::try_from(offset)
            .ok()
            .and_then(|offset| region.get(offset..))
            .ok_or(DescriptionError::Malformed)?;
        let tree = DeviceTree::from_bytes(blob).map_err(|_| DescriptionError::Malformed)?;
        Ok(Self { tree })
    }

    /// Доступные устройства в порядке описания. Устройство - доступный узел с непустым списком
    /// совместимостей. Узел со статусом, отличным от `okay` и `ok`, и его потомки устройствами
    /// не считаются.
    pub fn devices(&'a self) -> Devices<'a> {
        Devices {
            walk: self.tree.walk(),
            path: DeviceId::EMPTY,
            path_depth: 0,
            hidden_below: None,
        }
    }

    /// Доступные устройства, в списке совместимостей которых есть `compatible`, в порядке
    /// описания. Устройство, которое [`devices`](Self::devices) выдаёт ошибкой, пропускается.
    pub fn compatible_devices(
        &'a self,
        compatible: &'a str,
    ) -> impl Iterator<Item = Device<'a>> + 'a {
        self.parsed_devices()
            .filter(move |device| device.is_compatible(compatible))
    }

    /// Узел устройства `id`. Недоступный узел либо узел с недоступным предком даёт `None`.
    pub fn node(&'a self, id: &DeviceId) -> Option<Node<'a>> {
        available_path(&self.tree, id.as_str()).map(|found| Node { node: found.node() })
    }

    /// Узел по абсолютному пути `path` независимо от статуса узла и его предков. Компоненты пути
    /// сравниваются с именами узлов целиком. Путь длиннее [`BUS_PATH_MAX_NODES`] узлов даёт
    /// `None`.
    pub fn find(&'a self, path: &str) -> Option<Node<'a>> {
        node_path(&self.tree, path).map(|found| Node { node: found.node() })
    }

    fn parsed_devices(&'a self) -> impl Iterator<Item = Device<'a>> + 'a {
        self.devices().filter_map(Result::ok)
    }
}

impl<'a> Node<'a> {
    /// Значение свойства `name` как одно big-endian число из четырёх байт.
    pub fn u32(&self, name: &str) -> Result<Option<u32>, DescriptionError> {
        self.value(name, |value| value.try_into().ok().map(u32::from_be_bytes))
    }

    /// Значение свойства `name` как одно big-endian число из восьми байт.
    pub fn u64(&self, name: &str) -> Result<Option<u64>, DescriptionError> {
        self.value(name, |value| value.try_into().ok().map(u64::from_be_bytes))
    }

    /// Значение свойства `name` как строка до первого нуля.
    pub fn string(&self, name: &str) -> Result<Option<&'a str>, DescriptionError> {
        self.property(name, |property| property.as_cstr())
    }

    /// Значение свойства `name` как список непустых строк. Значение заканчивается нулём.
    pub fn strings(
        &self,
        name: &str,
    ) -> Result<Option<impl Iterator<Item = &'a str> + 'a>, DescriptionError> {
        self.property(name, PropExt::into_string_list_iter)
    }

    /// Байты значения свойства `name`.
    pub fn bytes(&self, name: &str) -> Option<&'a [u8]> {
        self.node.prop(name).map(|property| property.value())
    }

    fn value<T>(
        &self,
        name: &str,
        read: impl FnOnce(&'a [u8]) -> Option<T>,
    ) -> Result<Option<T>, DescriptionError> {
        self.property(name, |property| read(property.value()))
    }

    fn property<T>(
        &self,
        name: &str,
        read: impl FnOnce(devicetree::Property<'a>) -> Option<T>,
    ) -> Result<Option<T>, DescriptionError> {
        self.node
            .prop(name)
            .map(|property| read(property).ok_or(DescriptionError::InvalidValue))
            .transpose()
    }
}

impl<'a> Device<'a> {
    /// Путь узла устройства.
    pub fn id(&self) -> &DeviceId {
        &self.id
    }

    /// Совместимости устройства в порядке описания.
    pub fn compatible(&self) -> impl Iterator<Item = &'a str> + 'a {
        self.compatible.clone()
    }

    /// Есть ли `compatible` в списке совместимостей устройства.
    pub fn is_compatible(&self, compatible: &str) -> bool {
        self.compatible().any(|each| each == compatible)
    }
}

impl<'a> Iterator for Devices<'a> {
    type Item = Result<Device<'a>, DescriptionError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let (depth, node) = self.walk.next()?;
            if self.hidden_below.is_some_and(|hidden| depth > hidden) {
                continue;
            }
            self.hidden_below = None;
            if !node.is_available() {
                self.hidden_below = Some(depth);
                continue;
            }
            if depth == 0 {
                continue;
            }

            while self.path_depth >= depth {
                self.path.pop();
                self.path_depth -= 1;
            }
            if depth >= BUS_PATH_MAX_NODES || self.path.push(node.name()).is_none() {
                self.hidden_below = Some(depth);
                return Some(Err(DescriptionError::PathTooLong));
            }
            self.path_depth = depth;

            let Some(compatible) = node.prop("compatible") else {
                continue;
            };
            let Some(compatible) = compatible.into_string_list_iter() else {
                return Some(Err(DescriptionError::InvalidValue));
            };
            if compatible.clone().next().is_none() {
                continue;
            }
            return Some(Ok(Device {
                id: self.path.clone(),
                compatible,
            }));
        }
    }
}
