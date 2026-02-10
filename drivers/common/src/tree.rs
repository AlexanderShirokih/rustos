/// Абстракция свойства узла.
pub trait NodeProperty {
    fn name(&self) -> &str;

    fn raw(&self) -> &[u8];

    fn as_cstr(&self) -> Option<&str>;
}

/// Абстракция узла дерева устройств.
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

    fn children(&self) -> Self::ChildIter<'_>;
}

/// Абстракция источника дерева устройств.
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
