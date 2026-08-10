//! Агрегаты ответов: owned-структуры со скалярными полями. Значение агрегата занимает суб-кадр
//! одного поля ответа.

use crate::{cursor::DirCursor, descriptor::NodeDescriptor, kind::NodeKind};

ipc::wire_value! {
    /// Открытый дескриптор вместе с метаданными узла.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Opened {
        /// Дескриптор, закреплённый за открытым узлом.
        pub fd: NodeDescriptor,
        /// Вид узла.
        pub kind: NodeKind,
        /// Размер узла: у файла - длина содержимого в байтах, у каталога - число его записей.
        pub size: u64,
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
    /// Итог перечисления каталога в буфер данных сессии. Клиент повторяет вызов с `from = next`,
    /// пока не увидит `end`. `count == 0` без `end` значит, что следующая запись не помещается в
    /// буфер и его надо увеличить.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Listed {
        /// Число записей, уложенных в буфер.
        pub count: u32,
        /// Позиция, с которой продолжится перечисление.
        pub next: DirCursor,
        /// Каталог исчерпан: записей за `next` нет.
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
    }
}
