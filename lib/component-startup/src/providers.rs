use core::marker::PhantomData;

use component_abi::{Attributes, STARTUP_REQUEST_TIMEOUT, component_startup};
use kernel_objects::{Session, kind};
use syscall::Handle;

use crate::{
    StartupError,
    declare::{answered, request_session, start_parent},
    request::{Found, all, next, open},
};

/// Провайдеры требования `all`. Стартовый handle принадлежит процессу до его завершения.
pub struct Providers<P> {
    parent: Handle<kind::Port>,
    requirement: &'static str,
    protocol: PhantomData<P>,
}

/// Результаты поиска одного перечисления в порядке выдачи родителем. Отказ выдаётся
/// последним элементом перебора.
pub struct ProviderSessions {
    enumeration: Option<Session<component_startup::Protocol>>,
    requirement: &'static str,
}

impl<P: ipc::Protocol> Providers<P> {
    #[doc(hidden)]
    #[must_use]
    pub const fn new(parent: Handle<kind::Port>, requirement: &'static str) -> Self {
        Self {
            parent,
            requirement,
            protocol: PhantomData,
        }
    }

    /// Ищет экспорты протокола `P`, совпавшие с `attributes`. Набор фиксируется на момент
    /// вызова, и новый вызов повторяет поиск.
    pub fn all(&self, attributes: Attributes<'_>) -> Result<ProviderSessions, StartupError> {
        let parent = start_parent(self.parent).with_wait(STARTUP_REQUEST_TIMEOUT);
        let enumeration = answered(
            self.requirement,
            all::<P>(&parent, self.requirement, attributes),
        )?;

        Ok(ProviderSessions {
            enumeration: Some(enumeration),
            requirement: self.requirement,
        })
    }

    /// Открывает сессию экспорта результата `found`. Каждый запрос ждёт ответ не дольше
    /// `STARTUP_REQUEST_TIMEOUT` и повторяется при `IpcError::PeerClosed`.
    pub fn open(&self, found: &Found) -> Result<Session<P>, StartupError> {
        request_session(self.requirement, || {
            open::<P>(found, STARTUP_REQUEST_TIMEOUT)
        })
    }
}

impl Iterator for ProviderSessions {
    type Item = Result<Found, StartupError>;

    fn next(&mut self) -> Option<Self::Item> {
        let answer = next(self.enumeration.as_ref()?, STARTUP_REQUEST_TIMEOUT);
        let item = answered(self.requirement, answer).transpose();
        if !matches!(item, Some(Ok(_))) {
            self.enumeration = None;
        }
        item
    }
}
