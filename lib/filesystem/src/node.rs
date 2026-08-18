//! Идентичность узла внутри бекенда и его место в хранилище файлового сервера.

use crate::store::StoreId;

/// Ссылка на узел, действительная в пределах одного бекенда.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(u32);

/// Диапазон байт узла в хранилище: `offset` кратен странице, `size` - длина окна в байтах.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub store: StoreId,
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
