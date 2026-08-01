//! Идентичность узла внутри бекенда и его место в регионе точки монтирования.

/// Ссылка на узел, действительная в пределах одного бекенда.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(u32);

/// Диапазон байт узла в регионе точки монтирования: `offset` от начала региона
/// и кратен странице, `size` - фактическая длина данных.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub offset: u64,
    pub size: u64,
}

impl NodeId {
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u32 {
        self.0
    }
}
