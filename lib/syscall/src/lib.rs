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
//! | `0x01..=0x0F` | резерв (deprecated thread/process control)  |
//! | `0x10..=0x1F` | object base - общие операции на любом KO    |
//! | `0x20..=0x2F` | channel-специфичные                         |
//! | `0x30..=0x3F` | handle lifecycle                            |
//! | `0x40..=0x4F` | Process KObject                             |
//! | `0x50..=0x5F` | Thread KObject                              |
//! | `0x60..=0x6F` | Memory KObject                              |
//! | `0x70..=0x7F` | Mailbox KObject                             |
//!
//! # Memory KObject (`0x60..=0x6F`)
//!
//! | op    | Имя                       | Аргументы / возврат                                                                                |
//! |-------|---------------------------|----------------------------------------------------------------------------------------------------|
//! | 0x60  | `MemoryCreateVirtual`     | `size_bytes`, `access_mask` -> `region_h`                                                          |
//! | 0x61  | `MemoryCreatePhysical`    | `resource_h`, `pa`, `size_bytes`, `access_mask` -> `region_h`                                      |
//! | 0x63  | `MemoryMap`               | `region_h`, `size`, `flags` -> `va`                                                                |
//! | 0x64  | `MemoryRemap`             | `va`, `size`, `flags` -> `0`                                                                       |
//! | 0x65  | `MemoryAllocate`          | `size`, `flags` -> `va`                                                                            |
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
//! `ChannelWrite` (`0x21`) и `ChannelRead` (`0x22`) - register-flat,
//! 5 аргументов; `ChannelRead` упаковывает в возврат
//! `bytes_len | (handles_count << 32)`. Конкретные сигнатуры - на
//! `SyscallOp`.
//!
//! # Mailbox KObject (`0x70..=0x7F`)
//!
//! | op    | Имя                  | Аргументы / возврат                                                                  |
//! |-------|----------------------|--------------------------------------------------------------------------------------|
//! | 0x70  | `MailboxCreate`      | -> `mbox_h`                                                                          |
//! | 0x71  | `MailboxQueue`       | `mbox_h`, `packet_va`, `packet_len` (=32) -> `0`                                     |
//! | 0x72  | `MailboxWait`        | `mbox_h`, `timeout_ns`, `packet_va`, `packet_cap` (>=32) -> `packet_len` (=32)       |
//! | 0x73  | `MailboxWaitAsync`   | `mbox_h`, `target_h`, `key`, `mask | (mode << 32)` -> `0`                            |
//! | 0x74  | `MailboxCancel`      | `mbox_h`, `target_h`, `key` -> `0`                                                   |
//!
//! Зарезервированы, возвращают `BadSyscall`:
//!
//! | op          | Назначение |
//! |-------------|------------|
//! | 0x75..=0x7F | резерв     |
//!
//! `MailboxQueue`/`MailboxWait` копируют 32-байтный пакет (см.
//! `MAILBOX_PACKET_SIZE`) через user-VM;
//! user'у разрешено ставить только `User`-пакеты, signal-пакеты
//! резервированы для kernel-side observer'ов.
//!
//! Стабильность: набор и нумерация - часть ABI и не меняются произвольно.

#![cfg_attr(not(test), no_std)]

use core::num::NonZeroU32;

/// Capability процесса: непрозрачный идентификатор записи в его handle-table.
///
/// На syscall-ABI это ненулевой 32-битный HandleId; `0` зарезервирован под
/// невалидный handle и конструктором не принимается. Структуру значения
/// (generation/slot) интерпретирует только ядро.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
        match NonZeroU32::new(ret as u32) {
            Some(nz) => Ok(Self(nz)),
            None => Err(ret),
        }
    }
}

/// Закрытый набор поддерживаемых syscall-операций.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum SyscallOp {
    // 0x10..=0x1F - object base.
    ObjectSignal = 0x10,
    /// Ждёт сигналы KO. Аргументы: `arg0=handle`, `arg1=signals`
    /// (нижние 32 бита), `arg2=timeout_ns`; `timeout_ns == 0` -
    /// non-blocking poll. Возврат: observed-маска.
    ObjectWaitOne = 0x11,
    /// Ждёт сигналы на нескольких KO. Аргументы: `arg0=items_va`
    /// (массив 8-байтных записей `[handle: u32, mask: u32]` LE),
    /// `arg1=count`, `arg2=timeout_ns`. Primary возврат - observed-
    /// маска сработавшего KO, secondary - его индекс в `items`.
    ObjectWaitMany = 0x12,

    // 0x20..=0x2F - channel.
    ChannelCreate = 0x20,
    /// Помещает сообщение в парный endpoint. Аргументы:
    /// `arg0=handle`, `arg1=bytes_va`, `arg2=bytes_len`, `arg3=handles_va`,
    /// `arg4=handles_count`. Возвращает `0` на успехе.
    ChannelWrite = 0x21,
    /// Достаёт сообщение из inbound-очереди. Аргументы:
    /// `arg0=handle`, `arg1=bytes_va`, `arg2=bytes_cap`, `arg3=handles_va`,
    /// `arg4=handles_cap`. Возвращает упакованное `(bytes_len) |
    /// (handles_count << 32)` в основном регистре.
    ChannelRead = 0x22,

    // 0x30..=0x3F - handle lifecycle.
    HandleClose = 0x30,
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
    /// `Rights::MANAGE_PROCESS` на
    /// `process_handle`; для каждого региона - `MAP | (R/W/X по flags)`.
    /// Возврат `0`.
    ProcessLoadImage = 0x42,
    /// Финальный exit-код процесса. Аргументы: `arg0=handle`. Требует
    /// `Rights::INSPECT`.
    ProcessExitCode = 0x43,
    /// Завершает процесс: всем его потокам поднимает `THREAD_TERMINATED`,
    /// после декремента до нуля - `PROCESS_TERMINATED`. Аргументы:
    /// `arg0=handle`, `arg1=exit_code`. Требует
    /// `Rights::MANAGE_PROCESS`.
    ProcessTerminate = 0x44,
    /// Стартует первый поток уже загруженного образа и атомарно
    /// передаёт ему bootstrap-handles. Аргументы: `arg0=process_handle`,
    /// `arg1=entry_pc`, `arg2=user_sp`, `arg3=arg` (X0 первой
    /// инструкции), `arg4 = priority | (handles_count << 32)`,
    /// `arg5=handles_va` (массив `[u32; handles_count]` HandleId
    /// raw-значений). Требует
    /// `Rights::MANAGE_PROCESS` на
    /// `process_handle` и `Rights::TRANSFER`
    /// на каждом handle в `handles_va`. Возвращает handle на свежий
    /// `ThreadObject`.
    ProcessStart = 0x45,

    // 0x50..=0x5F - Thread KObject.
    /// Создаёт user-поток в указанном процессе. Аргументы:
    /// `arg0=process_handle`, `arg1=entry_pc`, `arg2=user_sp`, `arg3=arg`,
    /// `arg4=priority`. Требует
    /// `Rights::MANAGE_PROCESS` на
    /// `process_handle`.
    ThreadCreate = 0x50,
    /// Возвращает handle на собственный `ThreadObject`.
    ThreadSelf = 0x51,
    /// Завершает текущий поток. Аргумент: `arg0=exit_code`. Не возвращается.
    ThreadExit = 0x52,
    /// Финальный exit-код потока. Аргументы: `arg0=handle`. Требует
    /// `Rights::INSPECT`.
    ThreadExitCode = 0x53,
    /// Завершает указанный поток. Аргументы: `arg0=handle`,
    /// `arg1=exit_code`. Требует
    /// `Rights::MANAGE_THREAD`.
    /// Терминирование собственного потока через handle отвергается:
    /// для self-exit предусмотрен `Self::ThreadExit`.
    ThreadTerminate = 0x54,

    // 0x60..=0x6F - Memory KObject.
    /// Создаёт `KObject::Memory` с Virtual backing. Аргументы:
    /// `arg0=size_bytes`, `arg1=access_mask`. Возвращает `region_handle`.
    MemoryCreateVirtual = 0x60,
    /// Создаёт `KObject::Memory` с Physical backing. Аргументы:
    /// `arg0=resource_handle` на `PhysicalResource` (требует
    /// `Rights::MINT`), `arg1=pa`, `arg2=size_bytes`, `arg3=access_mask`.
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
    /// `arg0=size_bytes`, `arg1=flags_raw`. Возвращает базовый VA.
    MemoryAllocate = 0x65,
    /// Снимает маппинг и возвращает регион в free-list (Arc дропается; если
    /// последний - фреймы возвращаются в FA через `Drop` региона).
    /// Аргументы: `arg0=va`, `arg1=size_bytes`. Возврат `0`.
    MemoryFree = 0x66,
    /// Инспектирует Memory-регион. Аргумент: `arg0=region_handle`. Primary
    /// возврат - `size_bytes`, secondary - `(kind_tag << 16) | access_bits`.
    MemoryRegionInspect = 0x67,

    // 0x70..=0x7F - Mailbox KObject.
    /// Создаёт пустой `Mailbox`, регистрирует handle
    /// в текущей таблице и возвращает его сырой `HandleId`.
    MailboxCreate = 0x70,
    /// Кладёт пакет в очередь mailbox'а. Аргументы: `arg0=mbox_handle`,
    /// `arg1=packet_va` (user-указатель на 32-байтный пакет),
    /// `arg2=packet_len` (обязан быть равен
    /// `MAILBOX_PACKET_SIZE`). На полной
    /// очереди - `SyscallError::ShouldWait`.
    /// User-у разрешён только `User`-пакет; `kind != 0` отвергается как
    /// `InvalidArgument`.
    /// Требует `Rights::WRITE`.
    MailboxQueue = 0x71,
    /// Атомарно ждёт пакет и достаёт первый. Аргументы: `arg0=mbox_handle`,
    /// `arg1=timeout_ns` (`0` - non-blocking poll), `arg2=packet_va`
    /// (user-указатель на буфер >=32 B), `arg3=packet_cap` (>=32). Записывает
    /// 32 B пакета по `packet_va` и возвращает их количество. Требует
    /// `Rights::READ`.
    MailboxWait = 0x72,
    /// Подписывает mailbox на сигналы target'а. Аргументы:
    /// `arg0=mbox_handle`, `arg1=target_handle`, `arg2=key`,
    /// `arg3 = mask | (mode << 32)` (`mode == 0` - Once, `1` - Repeating).
    /// `target == mbox` отвергается как
    /// `InvalidArgument`.
    /// Требует `Rights::WRITE` на mbox и
    /// `Rights::WAIT` на target.
    MailboxWaitAsync = 0x73,
    /// Снимает подписку с парой `(target, key)`. Аргументы:
    /// `arg0=mbox_handle`, `arg1=target_handle`, `arg2=key`.
    /// Идемпотентен: отсутствующая подписка - `0`.
    MailboxCancel = 0x74,
}

impl SyscallOp {
    pub const fn from_raw(raw: u16) -> Option<Self> {
        match raw {
            0x10 => Some(Self::ObjectSignal),
            0x11 => Some(Self::ObjectWaitOne),
            0x12 => Some(Self::ObjectWaitMany),
            0x20 => Some(Self::ChannelCreate),
            0x21 => Some(Self::ChannelWrite),
            0x22 => Some(Self::ChannelRead),
            0x30 => Some(Self::HandleClose),
            0x31 => Some(Self::HandleDuplicate),
            0x40 => Some(Self::ProcessCreate),
            0x41 => Some(Self::ProcessSelf),
            0x42 => Some(Self::ProcessLoadImage),
            0x43 => Some(Self::ProcessExitCode),
            0x44 => Some(Self::ProcessTerminate),
            0x45 => Some(Self::ProcessStart),
            0x50 => Some(Self::ThreadCreate),
            0x51 => Some(Self::ThreadSelf),
            0x52 => Some(Self::ThreadExit),
            0x53 => Some(Self::ThreadExitCode),
            0x54 => Some(Self::ThreadTerminate),
            0x60 => Some(Self::MemoryCreateVirtual),
            0x61 => Some(Self::MemoryCreatePhysical),
            0x63 => Some(Self::MemoryMap),
            0x64 => Some(Self::MemoryRemap),
            0x65 => Some(Self::MemoryAllocate),
            0x66 => Some(Self::MemoryFree),
            0x67 => Some(Self::MemoryRegionInspect),
            0x70 => Some(Self::MailboxCreate),
            0x71 => Some(Self::MailboxQueue),
            0x72 => Some(Self::MailboxWait),
            0x73 => Some(Self::MailboxWaitAsync),
            0x74 => Some(Self::MailboxCancel),
            _ => None,
        }
    }
}

/// Бит сигнала канала "парный endpoint закрыт".
pub const CHANNEL_PEER_CLOSED: u32 = 1 << 1;

/// Бит сигнала "процесс завершён".
pub const PROCESS_TERMINATED: u32 = 1 << 0;

/// Длина пакета `MailboxQueue`/`MailboxWait` в байтах.
pub const MAILBOX_PACKET_SIZE: usize = 32;

/// Возврат syscall'а "операция должна быть повторена позже" (очередь полна и т.п.).
pub const SYSCALL_RETURN_SHOULD_WAIT: i64 = -7;

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
    fn from_raw_known_ops() {
        assert_eq!(SyscallOp::from_raw(0x10), Some(SyscallOp::ObjectSignal));
        assert_eq!(SyscallOp::from_raw(0x11), Some(SyscallOp::ObjectWaitOne));
        assert_eq!(SyscallOp::from_raw(0x12), Some(SyscallOp::ObjectWaitMany));
        assert_eq!(SyscallOp::from_raw(0x20), Some(SyscallOp::ChannelCreate));
        assert_eq!(SyscallOp::from_raw(0x21), Some(SyscallOp::ChannelWrite));
        assert_eq!(SyscallOp::from_raw(0x22), Some(SyscallOp::ChannelRead));
        assert_eq!(SyscallOp::from_raw(0x30), Some(SyscallOp::HandleClose));
        assert_eq!(SyscallOp::from_raw(0x31), Some(SyscallOp::HandleDuplicate));
        assert_eq!(SyscallOp::from_raw(0x40), Some(SyscallOp::ProcessCreate));
        assert_eq!(SyscallOp::from_raw(0x41), Some(SyscallOp::ProcessSelf));
        assert_eq!(SyscallOp::from_raw(0x42), Some(SyscallOp::ProcessLoadImage));
        assert_eq!(SyscallOp::from_raw(0x43), Some(SyscallOp::ProcessExitCode));
        assert_eq!(SyscallOp::from_raw(0x44), Some(SyscallOp::ProcessTerminate));
        assert_eq!(SyscallOp::from_raw(0x45), Some(SyscallOp::ProcessStart));
        assert_eq!(SyscallOp::from_raw(0x50), Some(SyscallOp::ThreadCreate));
        assert_eq!(SyscallOp::from_raw(0x51), Some(SyscallOp::ThreadSelf));
        assert_eq!(SyscallOp::from_raw(0x52), Some(SyscallOp::ThreadExit));
        assert_eq!(SyscallOp::from_raw(0x53), Some(SyscallOp::ThreadExitCode));
        assert_eq!(SyscallOp::from_raw(0x54), Some(SyscallOp::ThreadTerminate));
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
        assert_eq!(SyscallOp::from_raw(0x70), Some(SyscallOp::MailboxCreate));
        assert_eq!(SyscallOp::from_raw(0x71), Some(SyscallOp::MailboxQueue));
        assert_eq!(SyscallOp::from_raw(0x72), Some(SyscallOp::MailboxWait));
        assert_eq!(SyscallOp::from_raw(0x73), Some(SyscallOp::MailboxWaitAsync));
        assert_eq!(SyscallOp::from_raw(0x74), Some(SyscallOp::MailboxCancel));
    }

    #[test]
    fn from_raw_zero_is_none() {
        // 0x00 зарезервирован: трапы с обнулённым op-регистром не должны
        // случайно попадать в реальную операцию.
        assert_eq!(SyscallOp::from_raw(0), None);
    }

    #[test]
    fn from_raw_old_thread_exit_slot_is_none() {
        // ABI поломан: 0x01 более не означает thread_exit, а
        // зарезервирован.
        assert_eq!(SyscallOp::from_raw(0x01), None);
    }

    #[test]
    fn from_raw_unknown_op() {
        assert_eq!(SyscallOp::from_raw(3), None);
        assert_eq!(SyscallOp::from_raw(0x13), None);
        assert_eq!(SyscallOp::from_raw(0x32), None);
        assert_eq!(SyscallOp::from_raw(0x46), None);
        assert_eq!(SyscallOp::from_raw(0x55), None);
        assert_eq!(SyscallOp::from_raw(0x62), None);
        assert_eq!(SyscallOp::from_raw(0x68), None);
        assert_eq!(SyscallOp::from_raw(0x6F), None);
        assert_eq!(SyscallOp::from_raw(0x75), None);
        assert_eq!(SyscallOp::from_raw(0x76), None);
        assert_eq!(SyscallOp::from_raw(0x7F), None);
        assert_eq!(SyscallOp::from_raw(u16::MAX), None);
    }
}
