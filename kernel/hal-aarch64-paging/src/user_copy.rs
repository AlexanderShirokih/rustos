//! Проверка адреса и прав доступа к пользовательской памяти для копирования
//! через линейную карту ядра.

use memory::{memory_mapper::UserCopyError, virtual_address::VirtualAddress};

use crate::{
    linear_map::LinearMap,
    mapper::PageMapper,
    mem_flags::{Aarch64MemFlags, Access},
    memory_attribute::MemoryAttribute,
    table_alloc::TableAlloc,
};

/// Доступ к пользовательской памяти, который требуется ядру для копирования.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UserAccess {
    /// Чтение байта.
    Read,
    /// Запись байта.
    Write,
}

/// Возвращает адрес линейной карты ядра для виртуального адреса `va` в пространстве `user`.
///
/// Функция возвращает [`UserCopyError::NotMapped`], если `user` не отображает `va`.
/// Пользовательское отображение должно быть кэшируемым и разрешать EL0 доступ `access`.
/// Снимок `linear` должен содержать соответствующий физический адрес и разрешать EL1
/// тот же доступ. Функция возвращает [`UserCopyError::AccessDenied`] при нарушении этих
/// условий. Кэшируемый доступ к регистрам устройства нарушает правила доступа к памяти
/// устройств, а разные атрибуты кэширования одной физической страницы нарушают когерентность.
///
/// Если границы окна `linear` выровнены по странице, результат допускает `access` ко всей
/// 4К-странице, содержащей `va`.
pub fn kernel_alias<U: TableAlloc>(
    user: &PageMapper<U>,
    linear: &LinearMap,
    va: VirtualAddress,
    access: UserAccess,
) -> Result<VirtualAddress, UserCopyError> {
    let target = user.translate(va).map_err(|_| UserCopyError::NotMapped)?;
    if !opens_to_user(target.flags, access) || !is_cached(target.flags) {
        return Err(UserCopyError::AccessDenied);
    }
    linear
        .alias(target.pa, access)
        .ok_or(UserCopyError::AccessDenied)
}

fn is_cached(flags: Aarch64MemFlags) -> bool {
    flags.get_attr_index() == MemoryAttribute::Cached.index()
}

fn opens_to_user(flags: Aarch64MemFlags, access: UserAccess) -> bool {
    matches!(
        (flags.get_ap(), access),
        (Access::UserRW, _) | (Access::UserRO, UserAccess::Read)
    )
}
