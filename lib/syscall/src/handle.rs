use core::{fmt, marker::PhantomData, num::NonZeroU32};

use crate::{
    SyscallError,
    kind::{self, ObjectKind, WaitableKind},
};

/// Сырой HandleId syscall-ABI. Значение упаковывает номер слота таблицы в биты `[0..16)`
/// и поколение записи в биты `[16..32)`, `0` означает невалидный handle. Пара
/// проверяется ядром при обращении, несовпадение поколения даёт `BadHandle`.
pub type RawHandle = u32;

/// Ёмкость handle-таблицы процесса. Заполненная таблица отвечает `OutOfHandles`.
pub const HANDLE_TABLE_CAPACITY: u32 = 1024;

/// Граница длины массива `SignalWaitMany`: `count` вне `1..=WAIT_MANY_MAX_COUNT`
/// ядро отвергает как `InvalidArgument`.
pub const WAIT_MANY_MAX_COUNT: usize = 256;

/// Capability процесса: непрозрачный идентификатор записи в handle-таблице, помеченный
/// видом объекта `K`. Значение действительно только в таблице того процесса, который его
/// получил.
///
/// Метка `K` сохраняется обычными преобразованиями и не имеет представления в кадре.
/// Фактический вид объекта, права и поколение записи проверяет ядро при операции.
///
/// ```no_run
/// use syscall::{Handle, kind};
///
/// fn send(port: Handle<kind::Port>) -> Handle<kind::Port> {
///     port
/// }
///
/// let port = Handle::<kind::Port>::from_abi(core::num::NonZeroU32::new(7).unwrap());
/// assert_eq!(send(port).raw(), 7);
/// ```
///
/// Вид объекта одной операции не подставляется в операцию над другим видом:
///
/// ```compile_fail
/// use syscall::{Handle, kind};
///
/// fn send(port: Handle<kind::Port>) -> Handle<kind::Port> {
///     port
/// }
///
/// let region = Handle::<kind::MemoryRegion>::from_abi(core::num::NonZeroU32::new(7).unwrap());
/// let _ = send(region);
/// ```
///
/// Параметр вида обязателен и не имеет значения по умолчанию.
///
/// ```compile_fail
/// use syscall::Handle;
///
/// let _: Handle = Handle::from_abi(core::num::NonZeroU32::new(7).unwrap());
/// ```
#[repr(transparent)]
pub struct Handle<K: ObjectKind> {
    raw: NonZeroU32,
    kind: PhantomData<K>,
}

impl<K: ObjectKind> Handle<K> {
    /// Назначает виду `K` уже полученное сырое значение. Это низкоуровневая граница,
    /// и она не подтверждает заявленный вид. Фактический объект проверяет ядро при операции.
    pub const fn from_abi(raw: NonZeroU32) -> Self {
        Self {
            raw,
            kind: PhantomData,
        }
    }

    /// Разбирает знаковый возврат системного вызова:
    /// значение в `1..=u32::MAX` - валидный handle,
    /// иначе декодированный [`SyscallError`]. Возврат `0`, значение вне
    /// `-(u32::MAX)..=u32::MAX` и неизвестный код отказа дают `BadSyscall`.
    pub const fn from_syscall_return(ret: i64) -> Result<Self, SyscallError> {
        if ret >= 1 && ret <= u32::MAX as i64 {
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
            let handle = ret as u32;
            match NonZeroU32::new(handle) {
                Some(nz) => Ok(Self::from_abi(nz)),
                None => Err(SyscallError::BadSyscall),
            }
        } else {
            match SyscallError::from_return(ret) {
                Some(e) => Err(e),
                None => Err(SyscallError::BadSyscall),
            }
        }
    }

    /// Значение для укладки в аргумент syscall.
    pub const fn raw(self) -> u32 {
        self.raw.get()
    }

    pub const fn as_nonzero(self) -> NonZeroU32 {
        self.raw
    }

    /// Снимает статическую метку вида, сохраняя идентификатор.
    pub const fn erase(self) -> Handle<kind::Any> {
        Handle::from_abi(self.raw)
    }
}

impl<K: ObjectKind> Clone for Handle<K> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K: ObjectKind> Copy for Handle<K> {}

impl<K: ObjectKind> PartialEq for Handle<K> {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}

impl<K: ObjectKind> Eq for Handle<K> {}

impl<K: ObjectKind> fmt::Debug for Handle<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Handle").field(&self.raw).finish()
    }
}

/// Запись массива `SignalWaitMany`: capability target `handle` и маска ожидаемых сигналов
/// `mask`.
///
/// Раскладка записи в памяти - 8 байт, `handle` в `[0..4)` и `mask` в `[4..8)`, оба
/// little-endian. Ядро читает массив таких записей по указателю `items_va`. Повтор
/// одного handle в массиве допустим. Массив гетерогенен, и запись хранит handle без
/// статической метки вида.
///
/// ```no_run
/// use syscall::{Handle, WaitItem, kind};
/// use core::num::NonZeroU32;
///
/// let process = Handle::<kind::Process>::from_abi(NonZeroU32::new(3).unwrap());
/// let line = Handle::<kind::IrqLine>::from_abi(NonZeroU32::new(4).unwrap());
/// let items = [WaitItem::new(process, 1), WaitItem::new(line, 2)];
/// assert_eq!(items.len(), 2);
/// ```
///
/// Порт ядро в этой операции не принимает:
///
/// ```compile_fail
/// use syscall::{Handle, WaitItem, kind};
/// use core::num::NonZeroU32;
///
/// let port = Handle::<kind::Port>::from_abi(NonZeroU32::new(3).unwrap());
/// let _ = WaitItem::new(port, 1);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct WaitItem {
    handle: Handle<kind::Any>,
    mask: u32,
}

impl WaitItem {
    /// Собирает запись из `handle` ожидаемого вида и ненулевой маски сигналов `mask`.
    /// Нулевую маску ядро отвергает как `InvalidArgument`.
    pub const fn new<K: WaitableKind>(handle: Handle<K>, mask: u32) -> Self {
        Self {
            handle: handle.erase(),
            mask,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_from_abi_keeps_raw_id() {
        let raw = NonZeroU32::new(7).expect("nonzero");
        assert_eq!(Handle::<kind::Port>::from_abi(raw).raw(), 7);
        assert_eq!(Handle::<kind::Port>::from_abi(raw).erase().raw(), 7);
    }

    #[test]
    fn optional_handle_rejects_zero() {
        let absent = NonZeroU32::new(0).map(Handle::<kind::Port>::from_abi);
        assert!(absent.is_none());
        let present = NonZeroU32::new(7).map(Handle::<kind::Port>::from_abi);
        assert_eq!(present.map(Handle::raw), Some(7));
    }

    #[test]
    fn handle_from_syscall_return_splits_sign() {
        assert_eq!(
            Handle::<kind::Port>::from_syscall_return(5).map(Handle::raw),
            Ok(5)
        );
        assert_eq!(
            Handle::<kind::Port>::from_syscall_return(-7),
            Err(SyscallError::ShouldWait)
        );
        assert_eq!(
            Handle::<kind::Port>::from_syscall_return(0),
            Err(SyscallError::BadSyscall)
        );
        assert_eq!(
            Handle::<kind::Port>::from_syscall_return(i64::from(u32::MAX) + 1),
            Err(SyscallError::BadSyscall)
        );
    }

    /// Метка вида не занимает места: `PhantomData` не создаёт поля, а ниша `NonZeroU32`
    /// сохраняется у `Option`.
    #[test]
    fn handle_layout_matches_nonzero_for_every_kind() {
        assert_eq!(size_of::<Handle<kind::Port>>(), size_of::<NonZeroU32>());
        assert_eq!(align_of::<Handle<kind::Port>>(), align_of::<NonZeroU32>());
        assert_eq!(size_of::<Handle<kind::Any>>(), size_of::<NonZeroU32>());
        assert_eq!(align_of::<Handle<kind::Any>>(), align_of::<NonZeroU32>());
        assert_eq!(size_of::<Option<Handle<kind::Port>>>(), size_of::<u32>());
        assert_eq!(size_of::<Option<Handle<kind::Any>>>(), size_of::<u32>());
    }

    #[test]
    fn handles_of_one_kind_compare_by_id() {
        let raw = NonZeroU32::new(7).expect("nonzero");
        let other = NonZeroU32::new(8).expect("nonzero");
        assert_eq!(
            Handle::<kind::Port>::from_abi(raw),
            Handle::<kind::Port>::from_abi(raw)
        );
        assert_ne!(
            Handle::<kind::Port>::from_abi(raw),
            Handle::<kind::Port>::from_abi(other)
        );
    }

    #[test]
    fn wait_item_wire_layout() {
        assert_eq!(size_of::<WaitItem>(), 8);
        assert_eq!(align_of::<WaitItem>(), 4);
        assert_eq!(core::mem::offset_of!(WaitItem, handle), 0);
        assert_eq!(core::mem::offset_of!(WaitItem, mask), 4);
    }
}
