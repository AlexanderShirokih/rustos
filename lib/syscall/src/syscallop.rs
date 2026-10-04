/// Закрытый набор поддерживаемых syscall-операций.
///
/// Номер операции идёт immediate-полем ловушки, аргументы - в регистрах `arg0` и дальше
/// по порядку, результат - в `arg0`. Отказ приходит отрицательным `-(SyscallError)`,
/// парный результат кладётся вторым регистром. Номер вне этого набора даёт `BadSyscall`,
/// вызов из контекста ядра - `KernelOriginated`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum SyscallOp {
    // 0x10..=0x1F - signal base.
    /// Атомарно поднимает биты `set` и снимает биты `clear` на `Signal`.
    /// Аргументы:
    ///  - `arg0=handle`,
    ///  - `arg1=set` (нижние 32 бита),
    ///  - `arg2=clear` (нижние 32 бита),
    ///  - `arg3=wake_count` (`0` - None, `1` - One, `u64::MAX` - All,
    ///    прочее - `InvalidArgument`).
    ///
    /// Биты меняются при любом `wake_count`. Требует `Rights::WRITE`, принимает только
    /// `Signal`, на другом объекте - `WrongType`.
    /// Возврат `0`.
    SignalSet = 0x10,

    /// Ждёт сигналы capability target. Аргументы:
    ///  - `arg0=handle`,
    ///  - `arg1=signals` (нижние 32 бита, нулевая маска - `InvalidArgument`),
    ///  - `arg2=timeout_ns` (`timeout_ns == 0` - non-blocking poll).
    ///
    /// Требует `Rights::READ`. Объект без ожидаемого источника - `WrongType`. Закрытие
    /// либо передача handle до сигнала снимает ожидание кодом `Canceled`.
    /// Возврат: observed-маска (пересечение поднятых битов с `signals`).
    SignalWaitOne = 0x11,

    /// Ждёт сигналы на нескольких capability target. Аргументы:
    ///  - `arg0=items_va` (массив записей [`crate::WaitItem`]),
    ///  - `arg1=count` (вне `1..=`[`crate::WAIT_MANY_MAX_COUNT`] - `InvalidArgument`),
    ///  - `arg2=timeout_ns`.
    ///
    /// Требует `Rights::READ` на каждом handle записи, нулевая маска записи -
    /// `InvalidArgument`. Повтор одного handle в массиве допустим.
    /// Primary возврат - observed-маска сработавшего capability target,
    /// secondary - индекс этой записи в `items`.
    SignalWaitMany = 0x12,

    /// Создаёт `Signal` и регистрирует handle на него в таблице текущего процесса со
    /// стартовыми правами `READ | WRITE | DUPLICATE | TRANSFER`. Нет аргументов.
    /// Возврат: сырой `HandleId` этого handle.
    SignalCreate = 0x13,

    // 0x20..=0x2F - port (rendezvous-IPC).
    /// Создаёт `Port` и регистрирует один handle в текущей таблице со стартовыми правами
    /// `READ | WRITE | DUPLICATE | TRANSFER`. Обе роли порта берутся с этого handle
    /// дупликацией. Нет аргументов.
    /// Возврат: port handle id.
    PortCreate = 0x23,

    /// `send` на port: блокирующая отправка сообщения из IPC-буфера
    /// текущего потока. Аргументы:
    ///  - `arg0=handle`,
    ///  - `arg1=timeout_ns` ([`crate::PORT_TIMEOUT_INFINITE`] - бессрочно, `0` - poll,
    ///    иначе дедлайн в нс).
    ///
    /// Требует `Rights::WRITE`. Порт без получателей отвечает `PeerClosed` без блокировки.
    /// Перечисленные в `caps` handle требуют `Rights::TRANSFER` и уходят из таблицы
    /// отправителя на доставке, отказ оставляет их на месте.
    /// Возврат: `0`, [`crate::SYSCALL_RETURN_TIMEOUT`] при истечении тайм-аута,
    /// либо `-(SyscallError)`.
    PortSend = 0x24,

    /// `recv` на port: блокирующий приём в IPC-буфер текущего потока.
    /// Аргументы:
    ///  - `arg0=handle`,
    ///  - `arg1=timeout_ns` (см. `PortSend`).
    ///
    /// Требует `Rights::READ`. Значок отправителя ядро кладёт в `IpcBuffer.badge`,
    /// принятые handle вставляет в таблицу получателя.
    /// Возврат: reply handle id со стартовыми правами `WRITE | TRANSFER`, если встречный
    /// был `call` (иначе `0`), [`crate::SYSCALL_RETURN_TIMEOUT`] при истечении тайм-аута,
    /// либо `-(SyscallError)`.
    PortRecv = 0x25,

    /// `call` на port: блокирующий запрос-ответ. Сообщение из
    /// IPC-буфера текущего потока. Ответ оказывается там же. Аргументы:
    ///  - `arg0=handle`,
    ///  - `arg1=timeout_ns` (ограничивает всю операцию: ожидание
    ///    получателя + ожидание reply).
    ///
    /// Требует `Rights::WRITE`. Порт без получателей отвечает `PeerClosed` без блокировки.
    /// Уход по тайм-ауту оставляет IPC-буфер вызывателя нетронутым, а серверный
    /// `Self::PortReply` на таком кадре получает `PeerClosed`.
    /// Возврат: `0`, [`crate::SYSCALL_RETURN_TIMEOUT`] при истечении тайм-аута,
    /// либо `-(SyscallError)`.
    PortCall = 0x26,

    /// `reply` на одноразовый Reply-handle: доставляет ответ из IPC-буфера
    /// сервера вызывателю. Аргумент:
    ///  - `arg0=reply_handle`.
    ///
    /// Требует `Rights::WRITE`. Handle изымается из таблицы сервера и при отказе доставки
    /// тоже, повторный вызов на нём даёт `BadHandle`. Ушедший по тайм-ауту вызыватель
    /// даёт `PeerClosed`.
    /// Возврат: `0` либо `-(SyscallError)`.
    PortReply = 0x27,

    // 0x30..=0x3F - handle lifecycle.
    /// Изымает названный handle из таблицы текущего процесса. Аргументы:
    ///  - `arg0=handle`.
    ///
    /// Прав на handle не требует. Закрытие отзывает капы, выведенные из него, а объект
    /// освобождается с уходом последней ссылки. Неизвестный id - `BadHandle`.
    /// Возврат `0`.
    HandleClose = 0x30,

    /// Дублирует handle с подмножеством прав и (опционально) badge.
    /// Аргументы:
    ///  - `arg0=handle`,
    ///  - `arg1=new_rights` (нижние 32 бита, неизвестные позиции отбрасываются),
    ///  - `arg2=badge` (полные 64 бита). Значок set-once: заклеймить можно только
    ///    незаклеймённый источник. Заклеймённый наследует свой значок при
    ///    `badge == 0`, переклеймить (`badge != 0` на заклеймённом) - `BadHandle`.
    ///
    /// Требует `Rights::DUPLICATE`, `new_rights` вне прав источника - `AccessDenied`.
    /// Новый handle - потомок исходного по цепочке деривации, закрытие источника его
    /// отзывает.
    /// Возврат: новый handle id либо `-(SyscallError)`.
    HandleDuplicate = 0x31,

    // 0x40..=0x4F - Process CapabilityTarget.
    /// Создаёт пустой user-процесс. Аргументы:
    ///  - `arg0=name_va` (нулевой адрес - `InvalidArgument`),
    ///  - `arg1=name_len` (`1..=`[`crate::MAX_PROCESS_NAME_LEN`]).
    ///
    /// Ядро копирует имя из user-памяти до возврата, не-UTF-8 отвергает как
    /// `InvalidArgument`. У свежего процесса нет образа, за `ProcessCreate` идут
    /// `ProcessLoadImage` и `ProcessStart`.
    /// Возвращает handle на свежий `ProcessObject`.
    ProcessCreate = 0x40,

    /// Ставит в таблицу свежий handle на собственный `ProcessObject` со стартовыми правами
    /// `READ | WRITE | DUPLICATE | TRANSFER`. Нет аргументов. Каждый вызов занимает
    /// отдельный слот таблицы.
    ProcessSelf = 0x41,

    /// Устанавливает регионы образа в child AS и прикрепляет per-process
    /// user_vm-аллокатор. Аргументы:
    ///  - `arg0=process_handle`,
    ///  - `arg1=desc_va` (user-указатель на `UserImageDescAbi`),
    ///  - `arg2=desc_len` (равен [`crate::USER_IMAGE_DESC_SIZE`], иначе `InvalidArgument`).
    ///
    /// Требует `Rights::WRITE` на `process_handle`, для каждого региона -
    /// `WRITE | (R/W/X по flags)`. Ядро копирует дескриптор и массив сегментов из
    /// user-памяти до возврата, region-handle сегментов остаются у вызывающего.
    /// Возврат `0`.
    ProcessLoadImage = 0x42,

    /// Финальный exit-код процесса. Аргументы:
    ///  - `arg0=handle`.
    ///
    /// Требует `Rights::READ`. До завершения процесса возврат - `0`, и завершение
    /// отслеживается ожиданием бита `SIGNALED` на этом же handle.
    /// Возврат: exit code в нижних 32 битах.
    ProcessExitCode = 0x43,

    /// Завершает процесс. Помечает завершёнными все его потоки, после
    /// декремента до нуля - и сам процесс. Аргументы:
    ///  - `arg0=handle`,
    ///  - `arg1=exit_code` (нижние 32 бита).
    ///
    /// Требует `Rights::WRITE`. Терминирование собственного процесса через handle
    /// отвергается кодом `AccessDenied`, для self-exit предусмотрен `Self::ProcessExit`.
    /// Возврат `0`.
    ProcessTerminate = 0x44,

    /// Ставит свежий handle на метеринг-`Resource` текущего процесса. Нет аргументов.
    /// Права капы - те, с которыми ресурс достался процессу на старте, шире прав
    /// передавшего handle процессу не достаётся.
    /// `WrongType`, если процесс стартовал без метеринг-ресурса.
    /// Возвращает `resource_handle`.
    ProcessResourceSelf = 0x46,

    /// Выделяет часть бюджета `Resource` в отдельный ресурс с тем же потолком доступа.
    /// Аргументы:
    ///  - `arg0=resource_handle`,
    ///  - `arg1=budget_pages` (> 0).
    ///
    /// Требует `Rights::WRITE | Rights::DUPLICATE`. Страницы списываются с
    /// исходного ресурса и возвращаются ему, когда производный уходит.
    /// Нехватка бюджета и цепочка глубиной [`crate::MAX_RESOURCE_DEPTH`] дают
    /// `ResourceExhausted`. Производный ресурс - капа-потомок исходной, и отзыв исходной
    /// отзывает производную. Возвращает `resource_handle`.
    ResourceSplit = 0x47,

    /// Стартует первый поток уже загруженного образа и атомарно передаёт ему
    /// стартовый хэндл-канал. Аргументы:
    ///  - `arg0=process_handle`,
    ///  - `arg1=entry_pc`,
    ///  - `arg2=user_sp`,
    ///  - `arg3=bootstrap_handle` стартовый HandleId,
    ///  - `arg4=priority` (биты [0..8), [8..) = 0),
    ///  - `arg5=resource_handle` метеринг-ресурс потомка (0 - наследовать).
    ///
    /// X0 первого потока - child-table id переданного хэндла (канал процесса),
    /// вычисляется ядром после вставки в child-таблицу. Нулевой `bootstrap_handle`
    /// - `InvalidArgument`, `priority` со взведёнными битами `[8..)` - тоже.
    ///
    /// Требует `Rights::WRITE` на `process_handle`, `Rights::TRANSFER` на
    /// `bootstrap_handle` и `Rights::WRITE | Rights::DUPLICATE` на `resource_handle`.
    /// `bootstrap_handle` уходит из таблицы вызывающего в таблицу потомка без права
    /// `Rights::TRANSFER`, значок и остальные права сохраняются. Нулевой
    /// `resource_handle` отдаёт потомку метеринг-ресурс вызывающего с правами гранта
    /// вызывающего, ненулевой - права самого `resource_handle`.
    /// Возвращает handle на свежий `ThreadObject`.
    ProcessStart = 0x45,

    /// Завершает текущий процесс вместе со всеми его потоками. Аргументы:
    ///  - `arg0=exit_code` (нижние 32 бита).
    ///
    /// Не возвращается. Хэндла не требует. Завершение собственного процесса
    /// через `Self::ProcessTerminate` отвергается кодом `AccessDenied`.
    ProcessExit = 0x48,

    // 0x50..=0x5F - Thread CapabilityTarget.
    /// Создаёт user-поток в указанном процессе. Аргументы:
    ///  - `arg0=process_handle`,
    ///  - `arg1=entry_pc`,
    ///  - `arg2=user_sp`,
    ///  - `arg3=arg` (X0 первой инструкции),
    ///  - `arg4=priority` (биты `[0..8)`, значение свыше `0xFF` - `InvalidArgument`).
    ///
    /// Требует `Rights::WRITE` на `process_handle`. Процесс без загруженного образа
    /// отвечает `WrongType`.
    /// Возвращает handle на свежий `ThreadObject`.
    ThreadCreate = 0x50,

    /// Ставит в таблицу свежий handle на собственный `ThreadObject` со стартовыми правами
    /// `READ | WRITE | DUPLICATE | TRANSFER`. Нет аргументов. Каждый вызов занимает
    /// отдельный слот таблицы.
    ThreadSelf = 0x51,

    /// Завершает текущий поток. Аргумент:
    ///  - `arg0=exit_code` (нижние 32 бита).
    ///
    /// Не возвращается. На последнем потоке процесса завершается и процесс.
    ThreadExit = 0x52,

    /// Финальный exit-код потока. Аргументы:
    ///  - `arg0=handle`.
    ///
    /// Требует `Rights::READ`. До завершения потока возврат - `0`.
    /// Возврат: exit code в нижних 32 битах.
    ThreadExitCode = 0x53,

    /// Завершает указанный поток. Аргументы:
    ///  - `arg0=handle`,
    ///  - `arg1=exit_code` (нижние 32 бита).
    ///
    /// Требует `Rights::WRITE`.
    /// Терминирование собственного потока через handle отвергается кодом `AccessDenied`,
    /// для self-exit предусмотрен `Self::ThreadExit`.
    /// Возврат `0`.
    ThreadTerminate = 0x54,

    /// Возвращает user-VA per-thread IPC-буфера ([`crate::IpcBuffer`]) текущего
    /// потока. Нет аргументов. Буфер занимает страницу, замапленную user-RW, и у каждого
    /// потока он свой.
    /// Возврат: VA (>0) либо `WrongType` у потока ядра.
    ThreadIpcBufferAddr = 0x55,

    /// Уступает остаток кванта. Текущий поток встаёт в конец своей
    /// ready-очереди. Нет аргументов.
    ///
    /// Возврат `0`.
    ThreadYield = 0x56,

    // 0x60..=0x6F - Memory CapabilityTarget.
    /// Создаёт `CapabilityTarget::Memory` с Virtual backing. Аргументы:
    ///  - `arg0=resource_handle` (требует `Rights::WRITE`, метерится `size_bytes / PAGE` страниц),
    ///  - `arg1=size_bytes` (кратен [`crate::PAGE_SIZE`] и ненулевой),
    ///  - `arg2=access_mask` (ненулевая маска битов R/W/X),
    ///  - `arg3=region_flags` ([`crate::MEMORY_REGION_CONTIGUOUS`] либо ноль).
    ///
    /// Списанные страницы возвращаются ресурсу с уходом последней ссылки на регион.
    /// Нехватка бюджета - `ResourceExhausted`. Без свободного смежного участка для
    /// непрерывного региона вызов возвращает `OutOfMemory`.
    /// Возвращает `region_handle`.
    MemoryCreateVirtual = 0x60,

    /// Деривация узкого под-региона из Memory-региона. Аргументы:
    ///  - `arg0=region_handle` (требует `Rights::DUPLICATE`),
    ///  - `arg1=offset` (выровнен на страницу),
    ///  - `arg2=size_bytes`,
    ///  - `arg3=access_mask`,
    ///  - `arg4=region_flags` ([`crate::MEMORY_REGION_WRITE_COMBINING`] либо ноль для
    ///    профиля исходного региона).
    ///
    /// Окно `[offset, offset+size)` должно укладываться в исходный регион, доступ - не шире гранта.
    /// Грант - права handle, суженные access-маской региона, превышение даёт `AccessDenied`.
    /// Нарезка ничего не аллоцирует и бюджет не списывает. Под-регион - капа-потомок
    /// исходной. Возвращает `region_handle`.
    MemorySlice = 0x62,

    /// Маппит регион в текущий user-AS на свободный VA. Аргументы:
    ///  - `arg0=region_handle`,
    ///  - `arg1=size_bytes` (равен размеру региона, иначе `InvalidArgument`),
    ///  - `arg2=flags_raw` (см. `UserMemFlags`).
    ///
    /// Требует на handle права по `flags_raw`: `READ` для RO, `READ | WRITE` для RW,
    /// `READ | EXECUTE` для RX. Маппинг снимается отзывом `region_handle`, и капа,
    /// отозванная в гонке, оставляет отказ `Revoked` вместо живого VA.
    /// Возвращает базовый VA.
    MemoryMap = 0x63,

    /// Меняет флаги уже выделенного маппинга. Аргументы:
    ///  - `arg0=va` (база маппинга, выровнена на страницу),
    ///  - `arg1=size_bytes`,
    ///  - `arg2=flags_raw`.
    ///
    /// Регион остаётся прежним, handle на этом шаге не проверяется. Расширение доступа
    /// ограничено грантом, снятым при `Self::MemoryMap` либо `Self::MemoryAllocate`, и
    /// превышение даёт `InvalidArgument`. Неизвестный диапазон - `NotFound`.
    /// Возврат `0`.
    MemoryRemap = 0x64,

    /// Создаёт анонимный `Virtual` регион и сразу маппит его
    /// в свободный VA. `region_handle` не выкладывается. Аргументы:
    ///  - `arg0=resource_handle` (требует `Rights::WRITE`, метерится `size / PAGE` страниц),
    ///  - `arg1=size_bytes` (кратен [`crate::PAGE_SIZE`] и ненулевой),
    ///  - `arg2=flags_raw`.
    ///
    /// Доступ к маппингу задаётся `flags_raw` целиком, отзывать анонимный регион нечем.
    /// Бюджет возвращается ресурсу на `Self::MemoryFree` либо с разрушением AS.
    /// Возвращает базовый VA.
    MemoryAllocate = 0x65,

    /// Снимает маппинг и возвращает регион в free-list. Аргументы:
    ///  - `arg0=va` (база маппинга, выровнена на страницу),
    ///  - `arg1=size_bytes`.
    ///
    /// Невыделенный диапазон - `NotFound`. Уход последней ссылки на регион возвращает
    /// его страницы аллокатору фреймов и списанный бюджет ресурсу.
    /// Возврат `0`.
    MemoryFree = 0x66,

    /// Возвращает свойства Memory-региона. Аргумент:
    ///  - `arg0=region_handle`.
    ///
    /// Требует `Rights::READ`.
    /// Primary возврат - `size_bytes`,
    /// secondary - `base_pa | (profile_raw << 5) | (kind_tag << 3) | access_bits`
    /// (base_pa page-aligned, у непрерывного `Virtual` равен адресу первой страницы, у
    /// прочих `Virtual` равен 0). Младшие 12 бит base_pa свободны, и в них лежат `access_bits` в
    /// `[0..3)`, `kind_tag` в `[3..5)` и `profile_raw` в `[5..7)`.
    MemoryRegionInspect = 0x67,

    // 0x70..=0x7F - IRQ CapabilityTarget.
    /// Минтит `IrqLine` по полномочию `IrqControl`. Аргументы:
    ///  - `arg0=irq_control_handle` (требует `Rights::WRITE`),
    ///  - `arg1=irq` (номер линии в нижних 16 битах, должен попадать в диапазон полномочия),
    ///  - `arg2=trigger` (код [`crate::IrqTrigger`], неизвестный - `InvalidArgument`).
    ///
    /// Линия минтится один раз, повторный минт занятой линии - `AlreadyBound`. Номер вне
    /// диапазона полномочия - `AccessDenied`.
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
            0x47 => Some(Self::ResourceSplit),
            0x48 => Some(Self::ProcessExit),
            0x50 => Some(Self::ThreadCreate),
            0x51 => Some(Self::ThreadSelf),
            0x52 => Some(Self::ThreadExit),
            0x53 => Some(Self::ThreadExitCode),
            0x54 => Some(Self::ThreadTerminate),
            0x55 => Some(Self::ThreadIpcBufferAddr),
            0x56 => Some(Self::ThreadYield),
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
        assert_eq!(SyscallOp::from_raw(0x47), Some(SyscallOp::ResourceSplit));
        assert_eq!(SyscallOp::from_raw(0x48), Some(SyscallOp::ProcessExit));
        assert_eq!(SyscallOp::from_raw(0x50), Some(SyscallOp::ThreadCreate));
        assert_eq!(SyscallOp::from_raw(0x51), Some(SyscallOp::ThreadSelf));
        assert_eq!(SyscallOp::from_raw(0x52), Some(SyscallOp::ThreadExit));
        assert_eq!(SyscallOp::from_raw(0x53), Some(SyscallOp::ThreadExitCode));
        assert_eq!(SyscallOp::from_raw(0x54), Some(SyscallOp::ThreadTerminate));
        assert_eq!(SyscallOp::from_raw(0x56), Some(SyscallOp::ThreadYield));
        assert_eq!(
            SyscallOp::from_raw(0x60),
            Some(SyscallOp::MemoryCreateVirtual)
        );
        assert_eq!(SyscallOp::from_raw(0x62), Some(SyscallOp::MemorySlice));
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
        assert_eq!(SyscallOp::from_raw(0x68), None);
        assert_eq!(SyscallOp::from_raw(0x6F), None);
        assert_eq!(SyscallOp::from_raw(0x49), None);
        assert_eq!(SyscallOp::from_raw(0x72), None);
        assert_eq!(SyscallOp::from_raw(0x74), None);
        assert_eq!(SyscallOp::from_raw(0x75), None);
        assert_eq!(SyscallOp::from_raw(0x76), None);
        assert_eq!(SyscallOp::from_raw(0x7F), None);
        assert_eq!(SyscallOp::from_raw(0x80), None);
        assert_eq!(SyscallOp::from_raw(u16::MAX), None);
    }
}
