//! Владение capability стартовой транзакции.

use core::cell::RefCell;

use component_abi::{COMPONENT_EXPORT_MAX, COMPONENT_SLOT_MAX, receive_startup_transaction};
use ipc::{
    MessageLen, Timeout, Transport,
    wire::{Cap, IpcError},
};
use kernel_objects::{OwnedHandle, Port, PortTransport, ReceivedFrame, kind};
use syscall::{Handle, ObjectKind};

const TRANSACTION_FRAME_MAX: usize = COMPONENT_EXPORT_MAX + COMPONENT_SLOT_MAX + 1;

/// Отказ приёма стартовой транзакции.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupError {
    /// Одноразовый стартовый handle стандартной библиотеки уже выдан либо процесс запущен
    /// без него.
    MissingStartHandle,
    /// Состав транзакции объявлению компонента не отвечает.
    InvalidTransaction,
    /// Отказ чтения либо декодирования кадра.
    Transport(IpcError),
}

/// Принятая транзакция: лог-сессия компонента и ресурсы его объявления.
pub struct ReceivedStartup<T> {
    /// Делегированная лог-сессия компонента.
    pub log: Port,
    /// Startup-порт компонента, порты его экспортов и capability его именованных слотов.
    pub resources: T,
}

/// Владеющие capability принятой транзакции. Служебный тип, который разбирает раскрытие
/// [`component_manifest!`](crate::component_manifest).
#[doc(hidden)]
pub struct StartupSet {
    pub log: Port,
    pub startup: Port,
    exports: [Option<Port>; COMPONENT_EXPORT_MAX],
    taken_exports: usize,
    slots: [Option<OwnedHandle<kind::Any>>; COMPONENT_SLOT_MAX],
    taken_slots: usize,
}

struct Transaction<'a> {
    port: &'a PortTransport,
    frames: RefCell<[Option<ReceivedFrame<'a>>; TRANSACTION_FRAME_MAX]>,
}

impl StartupSet {
    /// Очередной порт экспорта в порядке объявления.
    pub fn next_export(&mut self) -> Result<Port, StartupError> {
        let export = self
            .exports
            .get_mut(self.taken_exports)
            .and_then(Option::take);
        self.taken_exports += 1;
        export.ok_or(StartupError::InvalidTransaction)
    }

    /// Очередной слот в порядке объявления. Вид `K` берётся из объявления этого слота в
    /// манифесте, и проверкой объекта ядром вызов не сопровождается.
    pub fn next_slot<K: ObjectKind>(&mut self) -> Result<OwnedHandle<K>, StartupError> {
        let slot = self.slots.get_mut(self.taken_slots).and_then(Option::take);
        self.taken_slots += 1;
        let slot = slot.ok_or(StartupError::InvalidTransaction)?;
        Ok(OwnedHandle::from_handle(Handle::from_abi(
            slot.into_raw().as_nonzero(),
        )))
    }
}

impl<'a> Transaction<'a> {
    fn over(port: &'a PortTransport) -> Self {
        Self {
            port,
            frames: RefCell::new(core::array::from_fn(|_| None)),
        }
    }

    fn hold(&self, frame: ReceivedFrame<'a>) {
        let mut frames = self.frames.borrow_mut();
        // Кадр сверх числа кадров контракта в транзакцию не складывается, и его ресурсы
        // закрываются вместе с ним.
        if let Some(place) = frames.iter_mut().find(|place| place.is_none()) {
            *place = Some(frame);
        }
    }

    fn claim<K: ObjectKind>(&self, cap: Cap<K>) -> Result<OwnedHandle<K>, StartupError> {
        self.frames
            .borrow_mut()
            .iter_mut()
            .flatten()
            .find_map(|frame| frame.claim(cap))
            .ok_or(StartupError::InvalidTransaction)
    }
}

impl Transport for Transaction<'_> {
    type Resources = ();

    fn take_resources(&self) {}

    fn write_message(&self, bytes: &[u8], handles: &[u32]) -> Result<(), IpcError> {
        self.port.write_message(bytes, handles)
    }

    fn read_message(&self, bytes: &mut [u8], handles: &mut [u32]) -> Result<MessageLen, IpcError> {
        let read = self.port.read_message(bytes, handles);
        // Ядро переносит capability кадра в таблицу процесса рандеву, до разбора кадра,
        // поэтому владение берётся сразу и отвергнутый кадр освобождает их по RAII.
        // Транзакция держит их до своего конца, и через транспорт они не выдаются.
        if let Some(frame) = ReceivedFrame::take(self.port) {
            self.hold(frame);
        }
        read
    }

    fn wait_readable(&self, timeout: Timeout) -> Result<(), IpcError> {
        self.port.wait_readable(timeout)
    }
}

/// Принимает транзакцию целиком по владеющему startup-порту и сверяет её состав с числом
/// объявленных экспортов и слотов. Порт остаётся у компонента, и по нему supervisor
/// запрашивает сессии его экспортов. Служебный вход, который вызывает раскрытие
/// [`component_manifest!`](crate::component_manifest).
#[doc(hidden)]
pub fn receive_set(
    startup: Port,
    exports: usize,
    slots: usize,
) -> Result<StartupSet, StartupError> {
    // Отказ приёма закрывает `startup` вместе с уже принятыми capability.
    let (log, exports, slots) = receive_over(&startup, exports, slots)?;
    Ok(StartupSet {
        log,
        startup,
        exports,
        taken_exports: 0,
        slots,
        taken_slots: 0,
    })
}

type ReceivedSet = (
    Port,
    [Option<Port>; COMPONENT_EXPORT_MAX],
    [Option<OwnedHandle<kind::Any>>; COMPONENT_SLOT_MAX],
);

fn receive_over(
    startup: &Port,
    declared_exports: usize,
    declared_slots: usize,
) -> Result<ReceivedSet, StartupError> {
    let declared_exports =
        u16::try_from(declared_exports).map_err(|_| StartupError::InvalidTransaction)?;
    let declared_slots =
        u16::try_from(declared_slots).map_err(|_| StartupError::InvalidTransaction)?;
    let port = PortTransport::server(startup);
    let transaction = Transaction::over(&port);
    let started = receive_startup_transaction(&transaction)
        .map_err(StartupError::Transport)?
        .ok_or(StartupError::InvalidTransaction)?;

    let mut wire = started
        .declared_exports(declared_exports)
        .ok_or(StartupError::InvalidTransaction)?;
    let mut exports: [Option<Port>; COMPONENT_EXPORT_MAX] = core::array::from_fn(|_| None);
    for place in exports.iter_mut().take(usize::from(declared_exports)) {
        let cap = wire.next().ok_or(StartupError::InvalidTransaction)?;
        *place = Some(transaction.claim(cap)?);
    }

    let mut wire = started
        .declared_slots(declared_slots)
        .ok_or(StartupError::InvalidTransaction)?;
    let mut slots: [Option<OwnedHandle<kind::Any>>; COMPONENT_SLOT_MAX] =
        core::array::from_fn(|_| None);
    for place in slots.iter_mut().take(usize::from(declared_slots)) {
        let cap = wire.next().ok_or(StartupError::InvalidTransaction)?;
        *place = Some(transaction.claim(cap)?);
    }

    Ok((transaction.claim(started.log)?, exports, slots))
}
