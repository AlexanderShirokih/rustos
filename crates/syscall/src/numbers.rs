//! Номера операций syscall-ABI.
//!
//! 16-битный код, выбранный платформой при входе в trap, отображается
//! в один из вариантов [`SyscallOp`]. Любой неизвестный код отвергается
//! с [`SyscallError::BadSyscall`](super::error::SyscallError::BadSyscall).
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
//! [`SyscallOp`].
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
//! [`MAILBOX_PACKET_SIZE`](kobject::MAILBOX_PACKET_SIZE)) через user-VM;
//! user'у разрешено ставить только `User`-пакеты, signal-пакеты
//! резервированы для kernel-side observer'ов.
//!
//! Стабильность: набор и нумерация - часть ABI и не меняются произвольно.

use super::error::SyscallError;

/// Закрытый набор поддерживаемых syscall-операций.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum SyscallOp {
    // 0x10..=0x1F - object base.
    ObjectSignal = 0x10,
    /// Ждёт сигналы KO. Аргументы: `arg0=handle`, `arg1=signals`
    /// (нижние 32 бита), `arg2=timeout_ns`; `timeout_ns == 0` -
    /// non-blocking poll, ненулевое значение - относительный timeout.
    ObjectWaitOne = 0x11,

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
    /// [`ProcessObject`](kobject::ProcessObject).
    ProcessCreate = 0x40,
    /// Возвращает handle на собственный [`ProcessObject`](kobject::ProcessObject).
    ProcessSelf = 0x41,
    /// Финальный exit-код процесса. Аргументы: `arg0=handle`. Требует
    /// [`Rights::INSPECT`](kobject::Rights::INSPECT).
    ProcessExitCode = 0x43,
    /// Завершает процесс: всем его потокам поднимает `THREAD_TERMINATED`,
    /// после декремента до нуля - `PROCESS_TERMINATED`. Аргументы:
    /// `arg0=handle`, `arg1=exit_code`. Требует
    /// [`Rights::MANAGE_PROCESS`](kobject::Rights::MANAGE_PROCESS).
    ProcessTerminate = 0x44,

    // 0x50..=0x5F - Thread KObject.
    /// Создаёт user-поток в указанном процессе. Аргументы:
    /// `arg0=process_handle`, `arg1=entry_pc`, `arg2=user_sp`, `arg3=arg`,
    /// `arg4=priority`. Требует
    /// [`Rights::MANAGE_PROCESS`](kobject::Rights::MANAGE_PROCESS) на
    /// `process_handle`.
    ThreadCreate = 0x50,
    /// Возвращает handle на собственный [`ThreadObject`](kobject::ThreadObject).
    ThreadSelf = 0x51,
    /// Завершает текущий поток. Аргумент: `arg0=exit_code`. Не возвращается.
    ThreadExit = 0x52,
    /// Финальный exit-код потока. Аргументы: `arg0=handle`. Требует
    /// [`Rights::INSPECT`](kobject::Rights::INSPECT).
    ThreadExitCode = 0x53,
    /// Завершает указанный поток. Аргументы: `arg0=handle`,
    /// `arg1=exit_code`. Требует
    /// [`Rights::MANAGE_THREAD`](kobject::Rights::MANAGE_THREAD).
    /// Терминирование собственного потока через handle отвергается:
    /// для self-exit предусмотрен [`Self::ThreadExit`].
    ThreadTerminate = 0x54,

    // 0x60..=0x6F - Memory KObject.
    /// Создаёт `KObject::Memory` с Virtual backing. Аргументы:
    /// `arg0=size_bytes`, `arg1=access_mask`. Возвращает `region_handle`.
    MemoryCreateVirtual = 0x60,
    /// Создаёт `KObject::Memory` с Physical backing. Аргументы:
    /// `arg0=resource_handle` на [`PhysicalResource`] (требует
    /// [`Rights::MINT`]), `arg1=pa`, `arg2=size_bytes`, `arg3=access_mask`.
    /// Возвращает `region_handle`.
    ///
    /// [`PhysicalResource`]: kobject::PhysicalResource
    /// [`Rights::MINT`]: kobject::Rights::MINT
    MemoryCreatePhysical = 0x61,
    /// Маппит регион в текущий user-AS на свободный VA. Аргументы:
    /// `arg0=region_handle`, `arg1=size_bytes`, `arg2=flags_raw`
    /// (см. [`UserMemFlags`](crate::UserMemFlags)). Возвращает базовый VA.
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
    /// Создаёт пустой [`Mailbox`](kobject::Mailbox), регистрирует handle
    /// в текущей таблице и возвращает его сырой `HandleId`.
    MailboxCreate = 0x70,
    /// Кладёт пакет в очередь mailbox'а. Аргументы: `arg0=mbox_handle`,
    /// `arg1=packet_va` (user-указатель на 32-байтный пакет),
    /// `arg2=packet_len` (обязан быть равен
    /// [`MAILBOX_PACKET_SIZE`](kobject::MAILBOX_PACKET_SIZE)). На полной
    /// очереди - [`SyscallError::ShouldWait`](super::error::SyscallError::ShouldWait).
    /// User-у разрешён только `User`-пакет; `kind != 0` отвергается как
    /// [`InvalidArgument`](super::error::SyscallError::InvalidArgument).
    /// Требует [`Rights::WRITE`](kobject::Rights::WRITE).
    MailboxQueue = 0x71,
    /// Атомарно ждёт пакет и достаёт первый. Аргументы: `arg0=mbox_handle`,
    /// `arg1=timeout_ns` (`0` - non-blocking poll), `arg2=packet_va`
    /// (user-указатель на буфер ≥32 B), `arg3=packet_cap` (≥32). Записывает
    /// 32 B пакета по `packet_va` и возвращает их количество. Требует
    /// [`Rights::READ`](kobject::Rights::READ).
    MailboxWait = 0x72,
    /// Подписывает mailbox на сигналы target'а. Аргументы:
    /// `arg0=mbox_handle`, `arg1=target_handle`, `arg2=key`,
    /// `arg3 = mask | (mode << 32)` (`mode == 0` - Once, `1` - Repeating).
    /// `target == mbox` отвергается как
    /// [`InvalidArgument`](super::error::SyscallError::InvalidArgument).
    /// Требует [`Rights::WRITE`](kobject::Rights::WRITE) на mbox и
    /// [`Rights::WAIT`](kobject::Rights::WAIT) на target.
    MailboxWaitAsync = 0x73,
    /// Снимает подписку с парой `(target, key)`. Аргументы:
    /// `arg0=mbox_handle`, `arg1=target_handle`, `arg2=key`.
    /// Идемпотентен: отсутствующая подписка - `0`.
    MailboxCancel = 0x74,
}

impl SyscallOp {
    pub const fn from_raw(raw: u16) -> Result<Self, SyscallError> {
        match raw {
            0x10 => Ok(Self::ObjectSignal),
            0x11 => Ok(Self::ObjectWaitOne),
            0x20 => Ok(Self::ChannelCreate),
            0x21 => Ok(Self::ChannelWrite),
            0x22 => Ok(Self::ChannelRead),
            0x30 => Ok(Self::HandleClose),
            0x31 => Ok(Self::HandleDuplicate),
            0x40 => Ok(Self::ProcessCreate),
            0x41 => Ok(Self::ProcessSelf),
            0x43 => Ok(Self::ProcessExitCode),
            0x44 => Ok(Self::ProcessTerminate),
            0x50 => Ok(Self::ThreadCreate),
            0x51 => Ok(Self::ThreadSelf),
            0x52 => Ok(Self::ThreadExit),
            0x53 => Ok(Self::ThreadExitCode),
            0x54 => Ok(Self::ThreadTerminate),
            0x60 => Ok(Self::MemoryCreateVirtual),
            0x61 => Ok(Self::MemoryCreatePhysical),
            0x63 => Ok(Self::MemoryMap),
            0x64 => Ok(Self::MemoryRemap),
            0x65 => Ok(Self::MemoryAllocate),
            0x66 => Ok(Self::MemoryFree),
            0x67 => Ok(Self::MemoryRegionInspect),
            0x70 => Ok(Self::MailboxCreate),
            0x71 => Ok(Self::MailboxQueue),
            0x72 => Ok(Self::MailboxWait),
            0x73 => Ok(Self::MailboxWaitAsync),
            0x74 => Ok(Self::MailboxCancel),
            _ => Err(SyscallError::BadSyscall),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_raw_known_ops() {
        assert_eq!(SyscallOp::from_raw(0x10), Ok(SyscallOp::ObjectSignal));
        assert_eq!(SyscallOp::from_raw(0x11), Ok(SyscallOp::ObjectWaitOne));
        assert_eq!(SyscallOp::from_raw(0x20), Ok(SyscallOp::ChannelCreate));
        assert_eq!(SyscallOp::from_raw(0x21), Ok(SyscallOp::ChannelWrite));
        assert_eq!(SyscallOp::from_raw(0x22), Ok(SyscallOp::ChannelRead));
        assert_eq!(SyscallOp::from_raw(0x30), Ok(SyscallOp::HandleClose));
        assert_eq!(SyscallOp::from_raw(0x31), Ok(SyscallOp::HandleDuplicate));
        assert_eq!(SyscallOp::from_raw(0x40), Ok(SyscallOp::ProcessCreate));
        assert_eq!(SyscallOp::from_raw(0x41), Ok(SyscallOp::ProcessSelf));
        assert_eq!(SyscallOp::from_raw(0x43), Ok(SyscallOp::ProcessExitCode));
        assert_eq!(SyscallOp::from_raw(0x44), Ok(SyscallOp::ProcessTerminate));
        assert_eq!(SyscallOp::from_raw(0x50), Ok(SyscallOp::ThreadCreate));
        assert_eq!(SyscallOp::from_raw(0x51), Ok(SyscallOp::ThreadSelf));
        assert_eq!(SyscallOp::from_raw(0x52), Ok(SyscallOp::ThreadExit));
        assert_eq!(SyscallOp::from_raw(0x53), Ok(SyscallOp::ThreadExitCode));
        assert_eq!(SyscallOp::from_raw(0x54), Ok(SyscallOp::ThreadTerminate));
        assert_eq!(
            SyscallOp::from_raw(0x60),
            Ok(SyscallOp::MemoryCreateVirtual)
        );
        assert_eq!(
            SyscallOp::from_raw(0x61),
            Ok(SyscallOp::MemoryCreatePhysical)
        );
        assert_eq!(SyscallOp::from_raw(0x63), Ok(SyscallOp::MemoryMap));
        assert_eq!(SyscallOp::from_raw(0x64), Ok(SyscallOp::MemoryRemap));
        assert_eq!(SyscallOp::from_raw(0x65), Ok(SyscallOp::MemoryAllocate));
        assert_eq!(SyscallOp::from_raw(0x66), Ok(SyscallOp::MemoryFree));
        assert_eq!(
            SyscallOp::from_raw(0x67),
            Ok(SyscallOp::MemoryRegionInspect)
        );
        assert_eq!(SyscallOp::from_raw(0x70), Ok(SyscallOp::MailboxCreate));
        assert_eq!(SyscallOp::from_raw(0x71), Ok(SyscallOp::MailboxQueue));
        assert_eq!(SyscallOp::from_raw(0x72), Ok(SyscallOp::MailboxWait));
        assert_eq!(SyscallOp::from_raw(0x73), Ok(SyscallOp::MailboxWaitAsync));
        assert_eq!(SyscallOp::from_raw(0x74), Ok(SyscallOp::MailboxCancel));
    }

    #[test]
    fn from_raw_zero_is_bad_syscall() {
        // 0x00 зарезервирован: трапы с обнулённым op-регистром не должны
        // случайно попадать в реальную операцию.
        assert_eq!(SyscallOp::from_raw(0), Err(SyscallError::BadSyscall));
    }

    #[test]
    fn from_raw_old_thread_exit_slot_is_bad_syscall() {
        // ABI поломан: 0x01 более не означает thread_exit, а
        // зарезервирован.
        assert_eq!(SyscallOp::from_raw(0x01), Err(SyscallError::BadSyscall));
    }

    #[test]
    fn from_raw_unknown_op() {
        assert_eq!(SyscallOp::from_raw(3), Err(SyscallError::BadSyscall));
        assert_eq!(SyscallOp::from_raw(0x12), Err(SyscallError::BadSyscall));
        assert_eq!(SyscallOp::from_raw(0x32), Err(SyscallError::BadSyscall));
        assert_eq!(SyscallOp::from_raw(0x42), Err(SyscallError::BadSyscall));
        assert_eq!(SyscallOp::from_raw(0x55), Err(SyscallError::BadSyscall));
        assert_eq!(SyscallOp::from_raw(0x62), Err(SyscallError::BadSyscall));
        assert_eq!(SyscallOp::from_raw(0x68), Err(SyscallError::BadSyscall));
        assert_eq!(SyscallOp::from_raw(0x6F), Err(SyscallError::BadSyscall));
        assert_eq!(SyscallOp::from_raw(0x75), Err(SyscallError::BadSyscall));
        assert_eq!(SyscallOp::from_raw(0x76), Err(SyscallError::BadSyscall));
        assert_eq!(SyscallOp::from_raw(0x7F), Err(SyscallError::BadSyscall));
        assert_eq!(SyscallOp::from_raw(u16::MAX), Err(SyscallError::BadSyscall));
    }
}
