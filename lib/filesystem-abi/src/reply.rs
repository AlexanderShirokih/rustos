//! Агрегаты ответов: owned-структуры со скалярными полями. Значение агрегата занимает
//! вложенную запись одного поля ответа.

use crate::{cursor::DirCursor, descriptor::NodeDescriptor, kind::NodeKind};

ipc::wire_value! {
    /// Открытый дескриптор вместе с метаданными узла.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Opened {
        /// Дескриптор, закреплённый за открытым узлом.
        pub fd: NodeDescriptor,
        /// Метаданные открытого узла.
        pub info: NodeInfo,
    }
}

ipc::wire_value! {
    /// Итог записи: сколько байт легло и по какому смещению.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Written {
        /// Число записанных байт.
        pub count: u32,
        /// Смещение, по которому легли байты.
        pub offset: u64,
    }
}

ipc::wire_value! {
    /// Итог пачки записей каталога. Клиент продолжает с позиции next до ответа с end.
    /// Нулевой count без end означает, что очередная запись не поместилась в окно.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Listed {
        /// Число записей, уложенных в буфер.
        pub count: u32,
        /// Позиция, с которой продолжится перечисление.
        pub next: DirCursor,
        /// Каталог исчерпан, и записей за `next` не осталось.
        pub end: bool,
    }
}

ipc::wire_value! {
    /// Метаданные узла.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct NodeInfo {
        /// Вид узла.
        pub kind: NodeKind,
        /// Размер узла: у файла - длина содержимого в байтах, у каталога - число его записей.
        pub size: u64,
        /// Номер тома: ненулевой и различный у томов, обслуживаемых одновременно.
        pub volume: u64,
        /// Номер узла в томе: неизменен, пока узел существует, и различен у живых узлов тома.
        pub node: u64,
    }
}
