//! Номера операций syscall-ABI.
//!
//! 16-битный код, выбранный платформой при входе в trap, отображается
//! в один из вариантов `SyscallOp`. Любой неизвестный код отвергается
//! с `SyscallError::BadSyscall`.
//!
//! # Группировка
//!
//! Номера группируются по высокому ниблу: операции одного класса лежат
//! в общем диапазоне, что упрощает добавление новых вариантов и оставляет
//! место под будущие миграции.
//!
//! | Диапазон      | Класс операций                              |
//! |---------------|---------------------------------------------|
//! | `0x00`        | резерв (`raw == 0` -> `BadSyscall`)          |
//! | `0x01..=0x0F` | резерв                                       |
//! | `0x10..=0x1F` | object base - Signal                                  |
//! | `0x20..=0x2F` | port-специфичные (rendezvous-IPC)       |
//! | `0x30..=0x3F` | handle lifecycle                            |
//! | `0x40..=0x4F` | Process KObject                             |
//! | `0x50..=0x5F` | Thread KObject                              |
//! | `0x60..=0x6F` | Memory KObject                              |
//!
//! # Memory KObject (`0x60..=0x6F`)
//!
//! | op    | Имя                       | Аргументы / возврат                                                                                |
//! |-------|---------------------------|----------------------------------------------------------------------------------------------------|
//! | 0x60  | `MemoryCreateVirtual`     | `resource_h`, `size_bytes`, `access_mask` -> `region_h`                                            |
//! | 0x61  | `MemoryCreatePhysical`    | `resource_h`, `pa`, `size_bytes`, `access_mask` -> `region_h`                                      |
//! | 0x63  | `MemoryMap`               | `region_h`, `size`, `flags` -> `va`                                                                |
//! | 0x64  | `MemoryRemap`             | `va`, `size`, `flags` -> `0`                                                                       |
//! | 0x65  | `MemoryAllocate`          | `resource_h`, `size`, `flags` -> `va`                                                              |
//! | 0x66  | `MemoryFree`              | `va`, `size` -> `0`                                                                                |
//! | 0x67  | `MemoryRegionInspect`     | `region_h` -> primary=`size_bytes`, secondary=`(kind_tag << 16) \| access_bits`                    |
//!
//! Зарезервированы, возвращают `BadSyscall`:
//!
//! | op    | Назначение |
//! |-------|------------|
//! | 0x62        | резерв |
//! | 0x68..=0x6F | резерв |
//!
//! Стабильность: набор и нумерация - часть ABI и не меняются произвольно.

#![cfg_attr(not(test), no_std)]

use core::num::NonZeroU32;

mod ipc_buffer;

pub use ipc_buffer::{IPC_BUFFER_DATA_MAX, IPC_BUFFER_MAX_CAPS, IpcBuffer, decode_tag, encode_tag};

/// Capability процесса: непрозрачный идентификатор записи в его handle-таблице.
///
/// На syscall-ABI это ненулевой 32-битный HandleId; `0` зарезервирован под
/// невалидный handle и конструктором не принимается. Структуру значения
/// (generation/slot) интерпретирует только ядро.
///
/// `repr(transparent)` фиксирует layout как у `u32`: `&[Handle]` и
/// `&[Option<Handle>]` (niche `0 == None`) совпадают с массивом `HandleId`,
/// который syscall-ABI читает и пишет по указателю.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct Handle(NonZeroU32);

impl Handle {
    /// Оборачивает сырой HandleId; `None` при нулевом значении.
    pub const fn new(raw: u32) -> Option<Self> {
        match NonZeroU32::new(raw) {
            Some(nz) => Some(Self(nz)),
            None => None,
        }
    }

    /// Сырое 32-битное значение для укладки в регистр или буфер syscall-ABI.
    pub const fn raw(self) -> u32 {
        self.0.get()
    }

    /// Разбирает знаковый возврат svc-обёртки, отдающей handle: значение в
    /// `1..=u32::MAX` - валидный handle, иначе `Err(ret)` (отрицательный
    /// `ret` несёт `-(SyscallError)`).
    pub const fn from_syscall_return(ret: i64) -> Result<Self, i64> {
        if ret < 1 || ret > u32::MAX as i64 {
            return Err(ret);
        }

        #[allow(clippy::cast_sign_loss)]
        let handle = ret as u32;
        match NonZeroU32::new(handle) {
            Some(nz) => Ok(Self(nz)),
            None => Err(ret),
        }
    }
}

/// Запись массива `SignalWaitMany`: KO `handle` и маска ожидаемых сигналов
/// `mask`. `#[repr(C)]` фиксирует wire-layout - 8 байт LE, `handle` в
/// `[0..4)`, `mask` в `[4..8)`, как читает ядро.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct WaitItem {
    handle: Handle,
    mask: u32,
}

impl WaitItem {
    /// Собирает запись из `handle` и ненулевой маски сигналов `mask`;
    /// нулевую маску ядро отвергает как `InvalidArgument`.
    pub const fn new(handle: Handle, mask: u32) -> Self {
        Self { handle, mask }
    }
}

/// Закрытый набор поддерживаемых syscall-операций.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum SyscallOp {
    // 0x10..=0x1F - object base.
    /// Меняет биты сигналов KO. Аргументы: `arg0=handle`, `arg1=set`
    /// (нижние 32 бита), `arg2=clear` (нижние 32 бита), `arg3=wake_count`
    /// (`WakeCount::to_raw`: 0=None, 1=One, 2=All; неизвестное -
    /// `InvalidArgument`). Биты выставляются всегда. Возврат `0`.
    SignalSet = 0x10,
    /// Ждёт сигналы KO. Аргументы: `arg0=handle`, `arg1=signals`
    /// (нижние 32 бита), `arg2=timeout_ns`; `timeout_ns == 0` -
    /// non-blocking poll. Возврат: observed-маска.
    SignalWaitOne = 0x11,
    /// Ждёт сигналы на нескольких KO. Аргументы: `arg0=items_va`
    /// (массив 8-байтных записей `[handle: u32, mask: u32]` LE),
    /// `arg1=count`, `arg2=timeout_ns`. Primary возврат - observed-
    /// маска сработавшего KO, secondary - его индекс в `items`.
    SignalWaitMany = 0x12,
    /// Создаёт пустой `Signal`, регистрирует handle в текущей
    /// таблице и возвращает его сырой `HandleId`. Аргументов нет.
    SignalCreate = 0x13,
    // 0x20..=0x2F - port (rendezvous-IPC).
    /// Создаёт `Port` (synchronous rendezvous-IPC) и регистрирует ОДИН
    /// handle в текущей таблице. Аргументов нет. Возврат: port handle id.
    PortCreate = 0x23,
    /// `send` на port: блокирующая отправка сообщения из IPC-буфера
    /// текущего потока. Аргументы: `arg0=handle`, `arg1=timeout_ns`
    /// ([`PORT_TIMEOUT_INFINITE`] - бессрочно, `0` - poll, иначе дедлайн в
    /// нс). Требует `Rights::WRITE`. Возврат: `0`, [`SYSCALL_RETURN_TIMEOUT`]
    /// при истечении тайм-аута, либо `-(SyscallError)`.
    PortSend = 0x24,
    /// `recv` на port: блокирующий приём в IPC-буфер текущего потока.
    /// Аргументы: `arg0=handle`, `arg1=timeout_ns` (см. `PortSend`). Требует
    /// `Rights::READ`. Возврат: reply handle id, если встречный был `call`
    /// (иначе `0`), [`SYSCALL_RETURN_TIMEOUT`] при истечении тайм-аута,
    /// либо `-(SyscallError)`.
    PortRecv = 0x25,
    /// `call` на port: блокирующий запрос-ответ. Сообщение из
    /// IPC-буфера текущего потока; ответ оказывается там же. Аргументы:
    /// `arg0=handle`, `arg1=timeout_ns` (ограничивает всю операцию: ожидание
    /// получателя + ожидание reply; см. `PortSend`). Требует `Rights::WRITE`.
    /// Возврат: `0`, [`SYSCALL_RETURN_TIMEOUT`] при истечении тайм-аута,
    /// либо `-(SyscallError)`.
    PortCall = 0x26,
    /// `reply` на одноразовый Reply-handle: доставляет ответ из IPC-буфера
    /// сервера вызывателю. Аргумент: `arg0=reply_handle`. Требует
    /// `Rights::WRITE`. Возврат: `0` либо `-(SyscallError)`.
    PortReply = 0x27,

    // 0x30..=0x3F - handle lifecycle.
    HandleClose = 0x30,
    /// Дублирует handle с подмножеством прав и (опционально) значком (badge).
    /// Аргументы: `arg0=handle`, `arg1=new_rights` (нижние 32 бита),
    /// `arg2=badge` (полные 64 бита). Значок set-once: заклеймить можно только
    /// незаклеймённый источник; заклеймённый наследует свой значок при
    /// `badge == 0`, переклеймить (`badge != 0` на заклеймённом) - `BadHandle`.
    /// Возврат: новый handle id либо `-(SyscallError)`.
    HandleDuplicate = 0x31,

    // 0x40..=0x4F - Process KObject.
    /// Создаёт пустой user-процесс. Аргументы: `arg0=name_va`,
    /// `arg1=name_len`. Возвращает handle на свежий
    /// `ProcessObject`.
    ProcessCreate = 0x40,
    /// Возвращает handle на собственный `ProcessObject`.
    ProcessSelf = 0x41,
    /// Устанавливает регионы образа в child AS и прикрепляет per-process
    /// user_vm-аллокатор. Аргументы: `arg0=process_handle`,
    /// `arg1=desc_va` (user-указатель на `UserImageDescAbi`),
    /// `arg2=desc_len` (== `USER_IMAGE_DESC_SIZE = 56`). Требует
    /// `Rights::WRITE` на
    /// `process_handle`; для каждого региона - `WRITE | (R/W/X по flags)`.
    /// Возврат `0`.
    ProcessLoadImage = 0x42,
    /// Финальный exit-код процесса. Аргументы: `arg0=handle`. Требует
    /// `Rights::READ`.
    ProcessExitCode = 0x43,
    /// Завершает процесс: помечает завершёнными все его потоки, после
    /// декремента до нуля - и сам процесс (bound-`Signal`'ы получают
    /// `SIGNALED`). Аргументы: `arg0=handle`, `arg1=exit_code`. Требует
    /// `Rights::WRITE`.
    ProcessTerminate = 0x44,
    /// Возвращает handle на bound-`Signal` терминации процесса (бит
    /// `SIGNALED`), материализуя его лениво. Аргумент: `arg0=handle`. Требует
    /// `Rights::READ`. Выданный handle - read-only (READ/DUPLICATE/TRANSFER).
    ProcessTerminationSignal = 0x46,
    /// Ставит свежий handle на метеринг-`Resource` текущего процесса
    /// (дефолтные права, включая `WRITE`). Аргументов нет. `WrongType`, если
    /// процесс стартовал без метеринг-ресурса. Возвращает `resource_handle`.
    ProcessResourceSelf = 0x47,
    /// Стартует первый поток уже загруженного образа и атомарно
    /// передаёт ему bootstrap-handles. Аргументы: `arg0=process_handle`,
    /// `arg1=entry_pc`, `arg2=user_sp`, `arg3=arg` (X0 первой
    /// инструкции), `arg4 = priority | (handles_count << 32)`,
    /// `arg5=handles_va` (массив `[u32; handles_count]` HandleId
    /// raw-значений). Требует `Rights::WRITE` на `process_handle` и
    /// `Rights::TRANSFER` на каждом handle в `handles_va`. Стартуемый
    /// процесс наследует метеринг-ресурс вызывающего. Возвращает handle на
    /// свежий `ThreadObject`.
    ProcessStart = 0x45,

    // 0x50..=0x5F - Thread KObject.
    /// Создаёт user-поток в указанном процессе. Аргументы:
    /// `arg0=process_handle`, `arg1=entry_pc`, `arg2=user_sp`, `arg3=arg`,
    /// `arg4=priority`. Требует
    /// `Rights::WRITE` на
    /// `process_handle`.
    ThreadCreate = 0x50,
    /// Возвращает handle на собственный `ThreadObject`.
    ThreadSelf = 0x51,
    /// Завершает текущий поток. Аргумент: `arg0=exit_code`. Не возвращается.
    ThreadExit = 0x52,
    /// Финальный exit-код потока. Аргументы: `arg0=handle`. Требует
    /// `Rights::READ`.
    ThreadExitCode = 0x53,
    /// Завершает указанный поток. Аргументы: `arg0=handle`,
    /// `arg1=exit_code`. Требует
    /// `Rights::WRITE`.
    /// Терминирование собственного потока через handle отвергается:
    /// для self-exit предусмотрен `Self::ThreadExit`.
    ThreadTerminate = 0x54,
    /// Возвращает handle на bound-`Signal` терминации потока (бит
    /// `SIGNALED`), материализуя его лениво. Аргумент: `arg0=handle`. Требует
    /// `Rights::READ`. Выданный handle - read-only (READ/DUPLICATE/TRANSFER).
    ThreadTerminationSignal = 0x55,
    /// Возвращает user-VA per-thread IPC-буфер ([`IpcBuffer`]) текущего
    /// потока. Аргументов нет. Возврат: VA (>0) либо `-(SyscallError)`,
    /// если у потока нет буфера (kernel-поток).
    IpcBufferAddr = 0x56,

    // 0x60..=0x6F - Memory KObject.
    /// Создаёт `KObject::Memory` с Virtual backing. Аргументы:
    /// `arg0=resource_handle` (требует `Rights::WRITE`; метерится
    /// `size_bytes / PAGE` страниц), `arg1=size_bytes`, `arg2=access_mask`.
    /// Возвращает `region_handle`.
    MemoryCreateVirtual = 0x60,
    /// Создаёт `KObject::Memory` с Physical backing. Аргументы:
    /// `arg0=resource_handle` на `Resource` (требует
    /// `Rights::WRITE`), `arg1=pa`, `arg2=size_bytes`, `arg3=access_mask`.
    /// Диапазон и доступ должны укладываться в границы ресурса.
    /// Возвращает `region_handle`.
    ///
    MemoryCreatePhysical = 0x61,
    /// Маппит регион в текущий user-AS на свободный VA. Аргументы:
    /// `arg0=region_handle`, `arg1=size_bytes`, `arg2=flags_raw`
    /// (см. `UserMemFlags`). Возвращает базовый VA.
    MemoryMap = 0x63,
    /// Меняет флаги уже выделенного маппинга (аналог `MemoryMapper::remap`).
    /// Аргументы: `arg0=va`, `arg1=size_bytes`, `arg2=flags_raw`. Возврат `0`.
    MemoryRemap = 0x64,
    /// Fastpath: создаёт анонимный `Virtual` регион и сразу маппит его
    /// в свободный VA. `region_handle` не выкладывается. Аргументы:
    /// `arg0=resource_handle` (требует `Rights::WRITE`; метерится
    /// `size / PAGE` страниц), `arg1=size_bytes`, `arg2=flags_raw`.
    /// Возвращает базовый VA.
    MemoryAllocate = 0x65,
    /// Снимает маппинг и возвращает регион в free-list (Arc дропается; если
    /// последний - фреймы возвращаются в FA через `Drop` региона).
    /// Аргументы: `arg0=va`, `arg1=size_bytes`. Возврат `0`.
    MemoryFree = 0x66,
    /// Инспектирует Memory-регион. Аргумент: `arg0=region_handle`. Primary
    /// возврат - `size_bytes`, secondary - `(kind_tag << 16) | access_bits`.
    MemoryRegionInspect = 0x67,
}

impl SyscallOp {
    pub const fn from_raw(raw: u16) -> Option<Self> {
        match raw {
            0x10 => Some(Self::SignalSet),
            0x11 => Some(Self::SignalWaitOne),
            0x12 => Some(Self::SignalWaitMany),
            0x13 => Some(Self::SignalCreate),
            0x23 => Some(Self::PortCreate),
            0x24 => Some(Self::PortSend),
            0x25 => Some(Self::PortRecv),
            0x26 => Some(Self::PortCall),
            0x27 => Some(Self::PortReply),
            0x30 => Some(Self::HandleClose),
            0x31 => Some(Self::HandleDuplicate),
            0x40 => Some(Self::ProcessCreate),
            0x41 => Some(Self::ProcessSelf),
            0x42 => Some(Self::ProcessLoadImage),
            0x43 => Some(Self::ProcessExitCode),
            0x44 => Some(Self::ProcessTerminate),
            0x45 => Some(Self::ProcessStart),
            0x46 => Some(Self::ProcessTerminationSignal),
            0x47 => Some(Self::ProcessResourceSelf),
            0x50 => Some(Self::ThreadCreate),
            0x51 => Some(Self::ThreadSelf),
            0x52 => Some(Self::ThreadExit),
            0x53 => Some(Self::ThreadExitCode),
            0x54 => Some(Self::ThreadTerminate),
            0x55 => Some(Self::ThreadTerminationSignal),
            0x56 => Some(Self::IpcBufferAddr),
            0x60 => Some(Self::MemoryCreateVirtual),
            0x61 => Some(Self::MemoryCreatePhysical),
            0x63 => Some(Self::MemoryMap),
            0x64 => Some(Self::MemoryRemap),
            0x65 => Some(Self::MemoryAllocate),
            0x66 => Some(Self::MemoryFree),
            0x67 => Some(Self::MemoryRegionInspect),
            _ => None,
        }
    }
}

/// Бит сигнала `Signal` "событие наступило". Единственный сигнальный бит,
/// используемый ядром; для bound-`Signal` терминации означает "завершён".
pub const SIGNALED: u32 = 1 << 0;

/// Политика пробуждения `SignalSet`: сколько ждущих будит смена битов.
/// Закрытый набор - произвольное N не выражается: будить часть ждущих при
/// уже выставленном бите оставило бы остальных спать с истинным условием
/// (lost-wakeup). Поддерживаются только края диапазона.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeCount {
    /// Только сменить биты, никого не будить (clear бита, взвод sticky-бита).
    None,
    /// Разбудить ровно одного ждущего в FIFO-порядке (unlock, notify_one).
    One,
    /// Разбудить всех пересекающихся ждущих (broadcast, notify_all).
    All,
}

impl WakeCount {
    /// Wire-кодировка для `arg3` `SignalSet`.
    pub const fn to_raw(self) -> u64 {
        match self {
            Self::None => 0,
            Self::One => 1,
            Self::All => 2,
        }
    }

    /// Декодирует `arg3`; `None` на неизвестном значении (нарушение ABI).
    pub const fn from_raw(raw: u64) -> Option<Self> {
        match raw {
            0 => Some(Self::None),
            1 => Some(Self::One),
            2 => Some(Self::All),
            _ => None,
        }
    }
}

/// Возврат syscall'а "операция должна быть повторена позже" (очередь полна,
/// нет встречной стороны и т.п.).
pub const SYSCALL_RETURN_SHOULD_WAIT: i64 = -7;

/// Возврат блокирующего Port-syscall'а "истёк тайм-аут ожидания"
/// (`-(SyscallError::Timeout)`). Возвращается при истечении `timeout_ns`, в
/// т.ч. в режиме poll (`timeout_ns == 0`), когда встречной стороны нет.
pub const SYSCALL_RETURN_TIMEOUT: i64 = -9;

/// ABI-значение `timeout_ns` блокирующих Port-syscall'ов "ждать бессрочно".
/// Sentinel `u64::MAX` отображается ядром в бессрочную блокировку (поведение
/// по умолчанию до введения тайм-аутов) и не занимает слот в sleeper-heap.
pub const PORT_TIMEOUT_INFINITE: u64 = u64::MAX;

/// ABI-значение `timeout_ns` блокирующих Port-syscall'ов "не блокироваться"
/// (poll): операция завершается немедленно, иначе возвращается
/// [`SYSCALL_RETURN_TIMEOUT`].
pub const PORT_TIMEOUT_POLL: u64 = 0;

/// `UserMemFlags::ReadWrite` в кодировке `flags_raw` для memory_map/allocate.
pub const MEM_FLAGS_READ_WRITE: u64 = 0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_new_rejects_zero() {
        assert!(Handle::new(0).is_none());
        assert_eq!(Handle::new(7).map(Handle::raw), Some(7));
    }

    #[test]
    fn handle_from_syscall_return_splits_sign() {
        assert_eq!(Handle::from_syscall_return(5).map(Handle::raw), Ok(5));
        assert_eq!(Handle::from_syscall_return(-7), Err(-7));
        assert_eq!(Handle::from_syscall_return(0), Err(0));
        assert_eq!(
            Handle::from_syscall_return(i64::from(u32::MAX) + 1),
            Err(i64::from(u32::MAX) + 1)
        );
    }

    #[test]
    fn wait_item_wire_layout() {
        assert_eq!(core::mem::size_of::<WaitItem>(), 8);
        assert_eq!(core::mem::align_of::<WaitItem>(), 4);
        assert_eq!(core::mem::offset_of!(WaitItem, handle), 0);
        assert_eq!(core::mem::offset_of!(WaitItem, mask), 4);
    }

    #[test]
    fn from_raw_known_ops() {
        assert_eq!(SyscallOp::from_raw(0x10), Some(SyscallOp::SignalSet));
        assert_eq!(SyscallOp::from_raw(0x11), Some(SyscallOp::SignalWaitOne));
        assert_eq!(SyscallOp::from_raw(0x12), Some(SyscallOp::SignalWaitMany));
        assert_eq!(SyscallOp::from_raw(0x13), Some(SyscallOp::SignalCreate));
        assert_eq!(SyscallOp::from_raw(0x56), Some(SyscallOp::IpcBufferAddr));
        assert_eq!(SyscallOp::from_raw(0x23), Some(SyscallOp::PortCreate));
        assert_eq!(SyscallOp::from_raw(0x24), Some(SyscallOp::PortSend));
        assert_eq!(SyscallOp::from_raw(0x25), Some(SyscallOp::PortRecv));
        assert_eq!(SyscallOp::from_raw(0x26), Some(SyscallOp::PortCall));
        assert_eq!(SyscallOp::from_raw(0x27), Some(SyscallOp::PortReply));
        assert_eq!(SyscallOp::from_raw(0x30), Some(SyscallOp::HandleClose));
        assert_eq!(SyscallOp::from_raw(0x31), Some(SyscallOp::HandleDuplicate));
        assert_eq!(SyscallOp::from_raw(0x40), Some(SyscallOp::ProcessCreate));
        assert_eq!(SyscallOp::from_raw(0x41), Some(SyscallOp::ProcessSelf));
        assert_eq!(SyscallOp::from_raw(0x42), Some(SyscallOp::ProcessLoadImage));
        assert_eq!(SyscallOp::from_raw(0x43), Some(SyscallOp::ProcessExitCode));
        assert_eq!(SyscallOp::from_raw(0x44), Some(SyscallOp::ProcessTerminate));
        assert_eq!(SyscallOp::from_raw(0x45), Some(SyscallOp::ProcessStart));
        assert_eq!(
            SyscallOp::from_raw(0x46),
            Some(SyscallOp::ProcessTerminationSignal)
        );
        assert_eq!(
            SyscallOp::from_raw(0x47),
            Some(SyscallOp::ProcessResourceSelf)
        );
        assert_eq!(SyscallOp::from_raw(0x50), Some(SyscallOp::ThreadCreate));
        assert_eq!(SyscallOp::from_raw(0x51), Some(SyscallOp::ThreadSelf));
        assert_eq!(SyscallOp::from_raw(0x52), Some(SyscallOp::ThreadExit));
        assert_eq!(SyscallOp::from_raw(0x53), Some(SyscallOp::ThreadExitCode));
        assert_eq!(SyscallOp::from_raw(0x54), Some(SyscallOp::ThreadTerminate));
        assert_eq!(
            SyscallOp::from_raw(0x55),
            Some(SyscallOp::ThreadTerminationSignal)
        );
        assert_eq!(
            SyscallOp::from_raw(0x60),
            Some(SyscallOp::MemoryCreateVirtual)
        );
        assert_eq!(
            SyscallOp::from_raw(0x61),
            Some(SyscallOp::MemoryCreatePhysical)
        );
        assert_eq!(SyscallOp::from_raw(0x63), Some(SyscallOp::MemoryMap));
        assert_eq!(SyscallOp::from_raw(0x64), Some(SyscallOp::MemoryRemap));
        assert_eq!(SyscallOp::from_raw(0x65), Some(SyscallOp::MemoryAllocate));
        assert_eq!(SyscallOp::from_raw(0x66), Some(SyscallOp::MemoryFree));
        assert_eq!(
            SyscallOp::from_raw(0x67),
            Some(SyscallOp::MemoryRegionInspect)
        );
    }

    #[test]
    fn from_raw_zero_is_none() {
        assert_eq!(SyscallOp::from_raw(0), None);
    }

    #[test]
    fn from_raw_unknown_op() {
        assert_eq!(SyscallOp::from_raw(3), None);
        assert_eq!(SyscallOp::from_raw(0x32), None);
        assert_eq!(SyscallOp::from_raw(0x57), None);
        assert_eq!(SyscallOp::from_raw(0x62), None);
        assert_eq!(SyscallOp::from_raw(0x68), None);
        assert_eq!(SyscallOp::from_raw(0x6F), None);
        assert_eq!(SyscallOp::from_raw(0x70), None);
        assert_eq!(SyscallOp::from_raw(0x74), None);
        assert_eq!(SyscallOp::from_raw(0x75), None);
        assert_eq!(SyscallOp::from_raw(0x76), None);
        assert_eq!(SyscallOp::from_raw(0x7F), None);
        assert_eq!(SyscallOp::from_raw(u16::MAX), None);
    }
}
