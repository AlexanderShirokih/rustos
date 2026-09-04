//! Владение capability стартовой транзакции.

use core::cell::RefCell;

use component_abi::{COMPONENT_SLOT_MAX, receive_startup_transaction};
use ipc::{
    MessageLen, Timeout, Transport,
    wire::{Cap, IpcError},
};
use kernel_objects::{OwnedHandle, PortTransport};
use syscall::{Handle, IPC_BUFFER_MAX_CAPS};

const ADOPTED_MAX: usize = (COMPONENT_SLOT_MAX + 1) * IPC_BUFFER_MAX_CAPS;

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

struct AdoptingTransport {
    port: PortTransport,
    adopted: RefCell<AdoptedCaps>,
}

struct AdoptedCaps {
    handles: [Option<OwnedHandle>; ADOPTED_MAX],
    count: usize,
}

impl StartupSet {
    /// Очередной слот в порядке объявления.
    pub fn next_slot(&mut self) -> Result<OwnedHandle, StartupError> {
        let slot = self.slots.get_mut(self.taken).and_then(Option::take);
        self.taken += 1;
        slot.ok_or(StartupError::InvalidTransaction)
    }
}

impl AdoptingTransport {
    fn server(startup: &OwnedHandle) -> Self {
        Self {
            port: PortTransport::server(startup),
            adopted: RefCell::new(AdoptedCaps::new()),
        }
    }

    fn into_adopted(self) -> AdoptedCaps {
        self.adopted.into_inner()
    }
}

impl Transport for AdoptingTransport {
    fn write_message(&self, bytes: &[u8], handles: &[u32]) -> Result<(), IpcError> {
        self.port.write_message(bytes, handles)
    }

    fn read_message(&self, bytes: &mut [u8], handles: &mut [u32]) -> Result<MessageLen, IpcError> {
        let read = self.port.read_message(bytes, handles);
        // Ядро переносит capability кадра в таблицу процесса рандеву, до разбора кадра,
        // поэтому владение берётся сразу и отвергнутый кадр освобождает их по RAII.
        let mut adopted = self.adopted.borrow_mut();
        for handle in self.port.staged_caps().into_iter().flatten() {
            adopted.adopt(handle);
        }
        read
    }

    fn wait_readable(&self, timeout: Timeout) -> Result<(), IpcError> {
        self.port.wait_readable(timeout)
    }
}

impl AdoptedCaps {
    fn new() -> Self {
        Self {
            handles: core::array::from_fn(|_| None),
            count: 0,
        }
    }

    fn adopt(&mut self, handle: Handle) {
        // Буфер IPC держит кадр до следующего рандеву, и неудавшийся приём оставляет в нём
        // capability предыдущего кадра, у которой уже есть владелец.
        if self.owns(handle) {
            return;
        }
        // Ёмкость набора покрывает базовый кадр и по кадру на слот, каждый до предела кадра
        // IPC. Кадр содержит capability сверх тех, что читает его wire-значение, и усыновляется
        // он целиком. Меньшая ёмкость закрыла бы capability, уже принятую приёмником за слот,
        // а транзакция сверх этой в контракт не складывается.
        let Some(place) = self.handles.get_mut(self.count) else {
            drop(OwnedHandle::from_handle(handle));
            return;
        };
        *place = Some(OwnedHandle::from_handle(handle));
        self.count += 1;
    }

    fn owns(&self, handle: Handle) -> bool {
        self.handles
            .iter()
            .flatten()
            .any(|owned| owned.as_raw().raw() == handle.raw())
    }

    fn take(&mut self, cap: Cap) -> Result<OwnedHandle, StartupError> {
        self.handles
            .iter_mut()
            .find(|place| {
                place
                    .as_ref()
                    .is_some_and(|owned| owned.as_raw().as_nonzero() == cap.raw())
            })
            .and_then(Option::take)
            .ok_or(StartupError::InvalidTransaction)
    }
}

/// Принимает транзакцию целиком по владеющему стартовому порту и сверяет её состав с числом
/// объявленных слотов. Служебный вход, который вызывает раскрытие
/// [`component_manifest!`](crate::component_manifest).
#[doc(hidden)]
pub fn receive_set(startup: OwnedHandle, declared: usize) -> Result<StartupSet, StartupError> {
    let declared = u16::try_from(declared).map_err(|_| StartupError::InvalidTransaction)?;
    let transport = AdoptingTransport::server(&startup);
    let started = receive_startup_transaction(&transport)
        .map_err(StartupError::Transport)?
        .ok_or(StartupError::InvalidTransaction)?;

    // Порт одноразовый, и по сложившейся транзакции второй попытки не будет.
    let mut adopted = transport.into_adopted();
    drop(startup);

    let mut wire = started
        .declared_slots(declared)
        .ok_or(StartupError::InvalidTransaction)?;
    let mut slots: [Option<OwnedHandle>; COMPONENT_SLOT_MAX] = core::array::from_fn(|_| None);
    for place in slots.iter_mut().take(usize::from(declared)) {
        let cap = wire.next().ok_or(StartupError::InvalidTransaction)?;
        *place = Some(adopted.take(cap)?);
    }

    Ok(StartupSet {
        log: adopted.take(started.log)?,
        control: adopted.take(started.control)?,
        service: adopted.take(started.service)?,
        slots,
        taken: 0,
    })
}
