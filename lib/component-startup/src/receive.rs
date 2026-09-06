//! Владение capability стартовой транзакции.

use core::cell::RefCell;

use component_abi::{COMPONENT_SLOT_MAX, receive_startup_transaction};
use ipc::{
    MessageLen, Timeout, Transport,
    wire::{Cap, IpcError},
};
use kernel_objects::{OwnedHandle, PortTransport, ReceivedFrame};

const TRANSACTION_FRAME_MAX: usize = COMPONENT_SLOT_MAX + 1;

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
    pub log: OwnedHandle,
    /// Порты компонента и capability его именованных слотов.
    pub resources: T,
}

/// Владеющие capability принятой транзакции. Служебный тип, который разбирает раскрытие
/// [`component_manifest!`](crate::component_manifest).
#[doc(hidden)]
pub struct StartupSet {
    pub log: OwnedHandle,
    pub control: OwnedHandle,
    pub service: OwnedHandle,
    slots: [Option<OwnedHandle>; COMPONENT_SLOT_MAX],
    taken: usize,
}

struct Transaction<'a> {
    port: &'a PortTransport,
    frames: RefCell<[Option<ReceivedFrame<'a>>; TRANSACTION_FRAME_MAX]>,
}

impl StartupSet {
    /// Очередной слот в порядке объявления.
    pub fn next_slot(&mut self) -> Result<OwnedHandle, StartupError> {
        let slot = self.slots.get_mut(self.taken).and_then(Option::take);
        self.taken += 1;
        slot.ok_or(StartupError::InvalidTransaction)
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

    fn claim(&self, cap: Cap) -> Result<OwnedHandle, StartupError> {
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

/// Принимает транзакцию целиком по владеющему стартовому порту и сверяет её состав с числом
/// объявленных слотов. Служебный вход, который вызывает раскрытие
/// [`component_manifest!`](crate::component_manifest).
#[doc(hidden)]
pub fn receive_set(startup: OwnedHandle, declared: usize) -> Result<StartupSet, StartupError> {
    let set = receive_over(&startup, declared);
    // Порт транзакции одноразовый, и второй попытки по нему не будет.
    drop(startup);
    set
}

fn receive_over(startup: &OwnedHandle, declared: usize) -> Result<StartupSet, StartupError> {
    let declared = u16::try_from(declared).map_err(|_| StartupError::InvalidTransaction)?;
    let port = PortTransport::server(startup);
    let transaction = Transaction::over(&port);
    let started = receive_startup_transaction(&transaction)
        .map_err(StartupError::Transport)?
        .ok_or(StartupError::InvalidTransaction)?;

    let mut wire = started
        .declared_slots(declared)
        .ok_or(StartupError::InvalidTransaction)?;
    let mut slots: [Option<OwnedHandle>; COMPONENT_SLOT_MAX] = core::array::from_fn(|_| None);
    for place in slots.iter_mut().take(usize::from(declared)) {
        let cap = wire.next().ok_or(StartupError::InvalidTransaction)?;
        *place = Some(transaction.claim(cap)?);
    }

    Ok(StartupSet {
        log: transaction.claim(started.log)?,
        control: transaction.claim(started.control)?,
        service: transaction.claim(started.service)?,
        slots,
        taken: 0,
    })
}
