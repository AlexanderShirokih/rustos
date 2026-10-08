use core::marker::PhantomData;

use component_abi::{Attributes, STARTUP_REQUEST_TIMEOUT};
use kernel_objects::{Session, kind};
use syscall::Handle;

use crate::{StartupError, declare::request_session, request::session};

/// Сессия требования `session`. Стартовый handle принадлежит процессу до его завершения.
pub struct Import<P> {
    parent: Handle<kind::Port>,
    requirement: &'static str,
    protocol: PhantomData<P>,
}

impl<P: ipc::Protocol> Import<P> {
    #[doc(hidden)]
    #[must_use]
    pub const fn new(parent: Handle<kind::Port>, requirement: &'static str) -> Self {
        Self {
            parent,
            requirement,
            protocol: PhantomData,
        }
    }

    /// Открывает сессию единственного экспорта протокола `P`, совпавшего с запросом без
    /// атрибутов. Каждый вызов посылает родителю новый запрос. Запрос ждёт ответ не дольше
    /// `STARTUP_REQUEST_TIMEOUT`, повторяется при `IpcError::PeerClosed` и заменяет ещё не
    /// принятый провайдером запрос того же требования.
    ///
    /// Отказ называет требование. Отсутствие совпадений даёт
    /// [`StartupFailure::Unbound`](crate::StartupFailure::Unbound).
    pub fn open(&self) -> Result<Session<P>, StartupError> {
        request_session(self.requirement, || {
            session::<P>(
                self.parent,
                self.requirement,
                Attributes::empty(),
                STARTUP_REQUEST_TIMEOUT,
            )
        })
    }
}
