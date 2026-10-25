use syscall::kind;

use crate::{error::BlockError, info::BlockInfo};

/// Наибольшее время, которое заявка на диск с живой сессией ждёт ухода этой сессии.
pub const SESSION_WAIT: ipc::Timeout = ipc::Timeout::from_secs(1);

ipc::protocol! {
    /// Диск, к которому открыта сессия. Диск обслуживает одну сессию одновременно.
    pub mod block_device {
        name = "BlockDevice";

        /// Возвращает информацию о диске.
        call fn info() -> BlockInfo;

        /// Выдаёт сессии RW-capability без права дублирования на буфер данных не меньше `size`
        /// байт. Регион принадлежит серверу, и закрытие сессии отзывает выданную capability.
        ///
        /// Отказы:
        /// - `AlreadyExists` - сессия уже получила буфер, прежний буфер остаётся у неё;
        /// - `NoSpace` - `size` нулевой, больше [`BlockInfo::transfer_max`] либо серверу не
        ///   хватает памяти.
        call fn acquire_buffer(size: u32) -> own Result<ipc::wire::Cap<kind::MemoryRegion>, BlockError>;

        /// Кладёт в начало буфера сессии `count` блоков диска начиная с блока `lba`. Нулевой
        /// `count` отвечает успехом без обращения к устройству.
        ///
        /// Отказы:
        /// - `NoBuffer` - у сессии нет буфера;
        /// - `BufferTooSmall` - `count` блоков длиннее `size`, с которым выделен буфер;
        /// - `OutOfRange` - `lba + count` больше числа блоков либо не помещается в `u64`;
        /// - `Device` - устройство ответило ошибкой, не ответило в срок либо диск в отказе.
        call async fn read(lba: u64, count: u32) -> Result<(), BlockError>;

        /// Записывает на диск `count` блоков из начала буфера сессии начиная с блока `lba`.
        /// Нулевой `count` отвечает успехом без обращения к устройству.
        ///
        /// Отказы те же, что у `read`.
        call async fn write(lba: u64, count: u32) -> Result<(), BlockError>;
    }
}
