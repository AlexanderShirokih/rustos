use kernel_objects::{MapError, MemoryRegion, ReadOnlyMapping};

use crate::{
    description::{Description, DescriptionRegion},
    error::DescriptionError,
};

/// Выданное описание платформы, отображённое только для чтения. Держит регион описания
/// отображением и выдаёт [`Description`] над его байтами.
#[derive(Debug)]
pub struct MappedDescription {
    mapping: ReadOnlyMapping,
    offset: u64,
}

impl MappedDescription {
    /// Отображает регион описания `description` и забирает его. Отказ отображения возвращает
    /// регион в [`MapError`], как [`ReadOnlyMapping::new`].
    pub fn new<M: Into<MemoryRegion>>(
        description: DescriptionRegion<M>,
    ) -> Result<Self, MapError<MemoryRegion>> {
        let mapping = ReadOnlyMapping::new(description.region.into())?;
        Ok(Self {
            mapping,
            offset: description.offset,
        })
    }

    /// Разбирает описание при каждом вызове, как [`Description::new`].
    pub fn description(&self) -> Result<Description<'_>, DescriptionError> {
        Description::new(self.mapping.as_bytes(), self.offset)
    }
}
