//! Метки вида объекта ядра для параметра `K` у [`Handle`](crate::Handle) и производных типов.
//!
//! Метка существует только на этапе компиляции, и её экземпляры не создаются. У вида нет
//! числового тега и таблицы регистрации. [`Any`] означает отсутствие статического знания о
//! виде.

mod sealed {
    pub trait Sealed {}
}

/// Замкнутое множество меток вида объекта. Реализация вне этого модуля недоступна.
pub trait ObjectKind: sealed::Sealed {}

/// Подмножество видов, которые ядро допускает в операциях ожидания сигналов.
pub trait WaitableKind: ObjectKind {}

macro_rules! declare_kinds {
    ($($(#[$meta:meta])* $name:ident),* $(,)?) => {
        $(
            $(#[$meta])*
            pub enum $name {}

            impl sealed::Sealed for $name {}

            impl ObjectKind for $name {}
        )*
    };
}

declare_kinds! {
    /// Объект сигнала.
    Signal,
    /// Процесс.
    Process,
    /// Поток.
    Thread,
    /// Область памяти; соответствует варианту `Memory` capability-таргета ядра.
    MemoryRegion,
    /// Ресурсный узел.
    Resource,
    /// Порт IPC.
    Port,
    /// Право ответить на принятый кадр порта.
    Reply,
    /// Полномочие выдавать линии прерываний.
    IrqControl,
    /// Линия прерывания.
    IrqLine,
    /// Отсутствие статического знания о виде объекта.
    Any,
}

impl WaitableKind for Signal {}

impl WaitableKind for Process {}

impl WaitableKind for Thread {}

impl WaitableKind for Port {}

impl WaitableKind for IrqLine {}
