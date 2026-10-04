/// Одно свойство узла дерева устройств: имя и сырые байты значения.
pub trait NodeProperty {
    fn name(&self) -> &str;

    fn raw(&self) -> &[u8];
}

/// Узел дерева устройств. Тип реализует `Copy`, и копия указывает на тот же узел тем же
/// лёгким хэндлом. Поддерево при копировании остаётся на месте.
pub trait DeviceNode: Copy {
    /// Тип идентификатора узла для ключей registry/map.
    type Id: Clone + Ord + Eq + core::fmt::Debug;

    type Property<'a>: NodeProperty
    where
        Self: 'a;

    type ChildIter<'a>: Iterator<Item = Self>
    where
        Self: 'a;

    fn id(&self) -> Self::Id;

    fn name(&self) -> &str;

    fn prop(&self, name: &str) -> Option<Self::Property<'_>>;

    /// Узел доступен, если у него нет свойства `status` либо его значение `okay` или `ok`.
    fn is_available(&self) -> bool;

    fn children(&self) -> Self::ChildIter<'_>;
}

/// Источник дерева устройств: корневой узел и перечисление всех узлов дерева.
pub trait DeviceTreeSource {
    type Node<'a>: DeviceNode
    where
        Self: 'a;

    type NodeIter<'a>: Iterator<Item = Self::Node<'a>>
    where
        Self: 'a;

    fn root(&self) -> Option<Self::Node<'_>>;

    fn nodes(&self) -> Self::NodeIter<'_>;
}
