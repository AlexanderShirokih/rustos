/// Закрытый набор поддерживаемых syscall-операций.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum SyscallOp {
    // 0x10..=0x1F - signal base.
    /// Меняет биты сигналов capability target.
    /// Аргументы:
    ///  - `arg0=handle`,
    ///  - `arg1=set` (нижние 32 бита),
    ///  - `arg2=clear` (нижние 32 бита),
    ///  - `arg3=wake_count` (0=None, 1=One, 2=All; неизвестное `InvalidArgument`).
    ///
    /// Возврат `0`.
    SignalSet = 0x10,

    /// Ждёт сигналы capability target. Аргументы:
    ///  - `arg0=handle`,
    ///  - `arg1=signals` (нижние 32 бита),
    ///  - `arg2=timeout_ns` (`timeout_ns == 0` - non-blocking poll).
    ///
    /// Возврат: observed-маска.
    SignalWaitOne = 0x11,

    /// Ждёт сигналы на нескольких capability target. Аргументы:
    ///  - `arg0=items_va` (массив 8-байтных записей `[handle: u32, mask: u32]` LE),
    ///  - `arg1=count`,
    ///  - `arg2=timeout_ns`.
    ///
    /// Primary возврат - observed-маска сработавшего capability target,
    /// secondary - его индекс в `items`.
    SignalWaitMany = 0x12,

    /// Создаёт пустой `Signal`, регистрирует handle в таблице и возвращает его сырой `HandleId`.
    /// Аргументов нет.
    SignalCreate = 0x13,

    // 0x20..=0x2F - port (rendezvous-IPC).
    /// Создаёт `Port` и регистрирует один handle в текущей таблице. Аргументов нет.
    /// Возврат: port handle id.
    PortCreate = 0x23,

    /// `send` на port: блокирующая отправка сообщения из IPC-буфера
    /// текущего потока. Аргументы:
    ///  - `arg0=handle`,
    ///  - `arg1=timeout_ns` ([`PORT_TIMEOUT_INFINITE`] - бессрочно, `0` - poll, иначе дедлайн в нс).
    ///
    /// Требует `Rights::WRITE`.
    /// Возврат: `0`, [`SYSCALL_RETURN_TIMEOUT`] при истечении тайм-аута, либо `-(SyscallError)`.
    PortSend = 0x24,

    /// `recv` на port: блокирующий приём в IPC-буфер текущего потока.
    /// Аргументы:
    ///  - `arg0=handle`,
    ///  - `arg1=timeout_ns` (см. `PortSend`).
    ///
    /// Требует `Rights::READ`.
    /// Возврат: reply handle id, если встречный был `call` (иначе `0`),
    /// [`SYSCALL_RETURN_TIMEOUT`] при истечении тайм-аута, либо `-(SyscallError)`.
    PortRecv = 0x25,

    /// `call` на port: блокирующий запрос-ответ. Сообщение из
    /// IPC-буфера текущего потока; ответ оказывается там же. Аргументы:
    ///  - `arg0=handle`,
    ///  - `arg1=timeout_ns` (ограничивает всю операцию: ожидание
    ///    получателя + ожидание reply).
    ///
    /// Требует `Rights::WRITE`.
    /// Возврат: `0`, [`SYSCALL_RETURN_TIMEOUT`] при истечении тайм-аута,
    /// либо `-(SyscallError)`.
    PortCall = 0x26,

    /// `reply` на одноразовый Reply-handle: доставляет ответ из IPC-буфера
    /// сервера вызывателю. Аргумент:
    ///  - `arg0=reply_handle`.
    ///
    /// Требует `Rights::WRITE`.
    /// Возврат: `0` либо `-(SyscallError)`.
    PortReply = 0x27,

    // 0x30..=0x3F - handle lifecycle.
    /// Закрывает текущий handle
    HandleClose = 0x30,

    /// Дублирует handle с подмножеством прав и (опционально) badge.
    /// Аргументы:
    ///  - `arg0=handle`,
    ///  - `arg1=new_rights` (нижние 32 бита),
    ///  - `arg2=badge` (полные 64 бита). Значок set-once: заклеймить можно только
    ///    незаклеймённый источник; заклеймённый наследует свой значок при
    ///    `badge == 0`, переклеймить (`badge != 0` на заклеймённом) - `BadHandle`.
    ///
    /// Возврат: новый handle id либо `-(SyscallError)`.
    HandleDuplicate = 0x31,

    // 0x40..=0x4F - Process CapabilityTarget.
    /// Создаёт пустой user-процесс. Аргументы:
    ///  - `arg0=name_va`,
    ///  - `arg1=name_len` (`1..=64`).
    ///
    /// Возвращает handle на свежий `ProcessObject`.
    ProcessCreate = 0x40,

    /// Возвращает handle на собственный `ProcessObject`.
    ProcessSelf = 0x41,

    /// Устанавливает регионы образа в child AS и прикрепляет per-process
    /// user_vm-аллокатор. Аргументы:
    ///  - `arg0=process_handle`,
    ///  - `arg1=desc_va` (user-указатель на `UserImageDescAbi`),
    ///  - `arg2=desc_len` (== `USER_IMAGE_DESC_SIZE = 56`).
    ///
    /// Требует `Rights::WRITE` на `process_handle`; для каждого региона - `WRITE | (R/W/X по flags)`.
    /// Возврат `0`.
    ProcessLoadImage = 0x42,

    /// Финальный exit-код процесса. Аргументы:
    ///  - `arg0=handle`.
    ///
    /// Требует `Rights::READ`.
    ProcessExitCode = 0x43,

    /// Завершает процесс: помечает завершёнными все его потоки, после
    /// декремента до нуля - и сам процесс. Аргументы:
    ///  - `arg0=handle`,
    ///  - `arg1=exit_code`.
    ///
    /// Требует `Rights::WRITE`.
    ProcessTerminate = 0x44,

    /// Ставит свежий handle на метеринг-`Resource` текущего процесса
    /// (дефолтные права, включая `WRITE`). Аргументов нет.
    /// `WrongType`, если процесс стартовал без метеринг-ресурса.
    /// Возвращает `resource_handle`.
    ProcessResourceSelf = 0x46,

    /// Стартует первый поток уже загруженного образа и атомарно передаёт ему
    /// стартовый хэндл-канал. Аргументы:
    ///  - `arg0=process_handle`,
    ///  - `arg1=entry_pc`,
    ///  - `arg2=user_sp`,
    ///  - `arg3=bootstrap_handle` стартовый HandleId,
    ///  - `arg4=priority` (биты [0..8), [8..) = 0),
    ///  - `arg5` - зарезервирован (0).
    ///
    /// X0 первого потока - child-table id переданного хэндла (канал процесса),
    /// вычисляется ядром после вставки в child-таблицу. Нулевой `bootstrap_handle`
    /// - `InvalidArgument`.
    ///
    /// Требует `Rights::WRITE` на `process_handle` и `Rights::TRANSFER` на
    /// `bootstrap_handle`. Стартуемый процесс наследует метеринг-ресурс
    /// вызывающего. Возвращает handle на свежий `ThreadObject`.
    ProcessStart = 0x45,

    // 0x50..=0x5F - Thread CapabilityTarget.
    /// Создаёт user-поток в указанном процессе. Аргументы:
    ///  - `arg0=process_handle`,
    ///  - `arg1=entry_pc`,
    ///  - `arg2=user_sp`,
    ///  - `arg3=arg`,
    ///  - `arg4=priority`.
    ///
    /// Требует `Rights::WRITE` на `process_handle`.
    ThreadCreate = 0x50,

    /// Возвращает handle на собственный `ThreadObject`.
    ThreadSelf = 0x51,

    /// Завершает текущий поток. Аргумент:
    ///  - `arg0=exit_code`.
    ///
    /// Не возвращается.
    ThreadExit = 0x52,

    /// Финальный exit-код потока. Аргументы:
    ///  - `arg0=handle`.
    ///
    /// Требует `Rights::READ`.
    ThreadExitCode = 0x53,

    /// Завершает указанный поток. Аргументы:
    ///  - `arg0=handle`,
    ///  - `arg1=exit_code`.
    ///
    /// Требует `Rights::WRITE`.
    /// Терминирование собственного потока через handle отвергается:
    /// для self-exit предусмотрен `Self::ThreadExit`.
    ThreadTerminate = 0x54,

    /// Возвращает user-VA per-thread IPC-буфер ([`IpcBuffer`]) текущего
    /// потока. Аргументов нет.
    /// Возврат: VA (>0) либо `-(SyscallError)`, если у потока нет буфера (kernel-поток).
    ThreadIpcBufferAddr = 0x55,

    // 0x60..=0x6F - Memory CapabilityTarget.
    /// Создаёт `CapabilityTarget::Memory` с Virtual backing. Аргументы:
    ///  - `arg0=resource_handle` (требует `Rights::WRITE`; метерится `size_bytes / PAGE` страниц),
    ///  - `arg1=size_bytes`,
    ///  - `arg2=access_mask`.
    ///
    /// Возвращает `region_handle`.
    MemoryCreateVirtual = 0x60,

    /// Деривация узкого под-региона из Memory-региона. Аргументы:
    ///  - `arg0=region_handle` (требует `Rights::DUPLICATE`),
    ///  - `arg1=offset` (выровнен на страницу),
    ///  - `arg2=size_bytes`,
    ///  - `arg3=access_mask`.
    ///
    /// Окно `[offset, offset+size)` должно укладываться в исходный регион, доступ - не шире гранта.
    /// Возвращает `region_handle`.
    MemorySlice = 0x62,

    /// Маппит регион в текущий user-AS на свободный VA. Аргументы:
    ///  - `arg0=region_handle`,
    ///  - `arg1=size_bytes`,
    ///  - `arg2=flags_raw` (см. `UserMemFlags`).
    ///
    /// Возвращает базовый VA.
    MemoryMap = 0x63,

    /// Меняет флаги уже выделенного маппинга. Аргументы:
    ///  - `arg0=va`,
    ///  - `arg1=size_bytes`,
    ///  - `arg2=flags_raw`.
    ///
    /// Возврат `0`.
    MemoryRemap = 0x64,

    /// Создаёт анонимный `Virtual` регион и сразу маппит его
    /// в свободный VA. `region_handle` не выкладывается. Аргументы:
    ///  - `arg0=resource_handle` (требует `Rights::WRITE`; метерится `size / PAGE` страниц),
    ///  - `arg1=size_bytes`,
    ///  - `arg2=flags_raw`.
    ///
    /// Возвращает базовый VA.
    MemoryAllocate = 0x65,

    /// Снимает маппинг и возвращает регион в free-list. Аргументы:
    ///  - `arg0=va`,
    ///  - `arg1=size_bytes`.
    ///
    /// Возврат `0`.
    MemoryFree = 0x66,

    /// Возвращает свойства Memory-региона. Аргумент:
    ///  - `arg0=region_handle`.
    ///
    /// Primary возврат - `size_bytes`,
    /// secondary - `base_pa | (kind_tag << 3) | access_bits`
    /// (base_pa page-aligned, младшие 12 бит несут kind/access; для `Virtual` base_pa = 0).
    MemoryRegionInspect = 0x67,

    // 0x70..=0x7F - IRQ CapabilityTarget.
    /// Минтит `IrqLine` по полномочию `IrqControl`. Аргументы:
    ///  - `arg0=irq_control_handle` (требует `Rights::WRITE`),
    ///  - `arg1=irq` (номер линии в нижних 16 битах; должен попадать в диапазон полномочия).
    ///
    /// Возвращает handle на свежий `IrqLine`. Срабатывание ожидается через
    /// `SignalWaitOne`/`SignalWaitMany` прямо по этому handle (бит `SIGNALED`).
    IrqMint = 0x70,

    /// Подтверждает прерывание на `IrqLine`: снимает latch `SIGNALED` и
    /// размаскирует линию. Аргумент:
    ///  - `arg0=irq_line_handle`.
    ///
    /// Требует `Rights::WRITE`.
    /// Возврат `0`.
    IrqAck = 0x71,
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
            0x46 => Some(Self::ProcessResourceSelf),
            0x50 => Some(Self::ThreadCreate),
            0x51 => Some(Self::ThreadSelf),
            0x52 => Some(Self::ThreadExit),
            0x53 => Some(Self::ThreadExitCode),
            0x54 => Some(Self::ThreadTerminate),
            0x55 => Some(Self::ThreadIpcBufferAddr),
            0x60 => Some(Self::MemoryCreateVirtual),
            0x62 => Some(Self::MemorySlice),
            0x63 => Some(Self::MemoryMap),
            0x64 => Some(Self::MemoryRemap),
            0x65 => Some(Self::MemoryAllocate),
            0x66 => Some(Self::MemoryFree),
            0x67 => Some(Self::MemoryRegionInspect),
            0x70 => Some(Self::IrqMint),
            0x71 => Some(Self::IrqAck),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_raw_known_ops() {
        assert_eq!(SyscallOp::from_raw(0x10), Some(SyscallOp::SignalSet));
        assert_eq!(SyscallOp::from_raw(0x11), Some(SyscallOp::SignalWaitOne));
        assert_eq!(SyscallOp::from_raw(0x12), Some(SyscallOp::SignalWaitMany));
        assert_eq!(SyscallOp::from_raw(0x13), Some(SyscallOp::SignalCreate));
        assert_eq!(
            SyscallOp::from_raw(0x55),
            Some(SyscallOp::ThreadIpcBufferAddr)
        );
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
            Some(SyscallOp::ProcessResourceSelf)
        );
        assert_eq!(SyscallOp::from_raw(0x50), Some(SyscallOp::ThreadCreate));
        assert_eq!(SyscallOp::from_raw(0x51), Some(SyscallOp::ThreadSelf));
        assert_eq!(SyscallOp::from_raw(0x52), Some(SyscallOp::ThreadExit));
        assert_eq!(SyscallOp::from_raw(0x53), Some(SyscallOp::ThreadExitCode));
        assert_eq!(SyscallOp::from_raw(0x54), Some(SyscallOp::ThreadTerminate));
        assert_eq!(
            SyscallOp::from_raw(0x60),
            Some(SyscallOp::MemoryCreateVirtual)
        );
        assert_eq!(SyscallOp::from_raw(0x62), Some(SyscallOp::MemorySlice));
        assert_eq!(SyscallOp::from_raw(0x61), None);
        assert_eq!(SyscallOp::from_raw(0x63), Some(SyscallOp::MemoryMap));
        assert_eq!(SyscallOp::from_raw(0x64), Some(SyscallOp::MemoryRemap));
        assert_eq!(SyscallOp::from_raw(0x65), Some(SyscallOp::MemoryAllocate));
        assert_eq!(SyscallOp::from_raw(0x66), Some(SyscallOp::MemoryFree));
        assert_eq!(
            SyscallOp::from_raw(0x67),
            Some(SyscallOp::MemoryRegionInspect)
        );
        assert_eq!(SyscallOp::from_raw(0x70), Some(SyscallOp::IrqMint));
        assert_eq!(SyscallOp::from_raw(0x71), Some(SyscallOp::IrqAck));
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
        assert_eq!(SyscallOp::from_raw(0x61), None);
        assert_eq!(SyscallOp::from_raw(0x68), None);
        assert_eq!(SyscallOp::from_raw(0x6F), None);
        assert_eq!(SyscallOp::from_raw(0x47), None);
        assert_eq!(SyscallOp::from_raw(0x56), None);
        assert_eq!(SyscallOp::from_raw(0x72), None);
        assert_eq!(SyscallOp::from_raw(0x74), None);
        assert_eq!(SyscallOp::from_raw(0x75), None);
        assert_eq!(SyscallOp::from_raw(0x76), None);
        assert_eq!(SyscallOp::from_raw(0x7F), None);
        assert_eq!(SyscallOp::from_raw(u16::MAX), None);
    }
}
