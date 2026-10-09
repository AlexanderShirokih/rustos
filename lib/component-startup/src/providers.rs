use core::marker::PhantomData;

use component_abi::{Attributes, STARTUP_REQUEST_TIMEOUT, component_startup};
use ipc::Timeout;
use kernel_objects::{Session, kind};
use syscall::Handle;

use crate::{
    StartupError, StartupFailure,
    declare::{answered, request_session, start_parent},
    request::{Found, all, next, open, watch},
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

/// Наблюдение за экспортами требования `all`. Наблюдение выдаёт совпавший экспорт, когда тот
/// становится видимым, по одному разу на экспорт и снова после перезапуска провайдера.
pub struct ProviderWatch {
    watch: Session<component_startup::Protocol>,
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

    /// Запрашивает наблюдение за экспортами протокола `P`, совпавшими с `attributes`. Запрос
    /// ждёт ответ не дольше `STARTUP_REQUEST_TIMEOUT`.
    pub fn watch(&self, attributes: Attributes<'_>) -> Result<ProviderWatch, StartupError> {
        let parent = start_parent(self.parent).with_wait(STARTUP_REQUEST_TIMEOUT);
        let watch = answered(
            self.requirement,
            watch::<P>(&parent, self.requirement, attributes),
        )?;

        Ok(ProviderWatch {
            watch,
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

impl ProviderWatch {
    /// Ждёт не дольше `wait` экспорт, ещё не выданный этим наблюдением.
    ///
    /// Отказы:
    /// - `Ipc(IpcError::Timeout)` - срок истёк. Экспорт, ставший видимым позже, выдаст
    ///   следующий вызов;
    /// - `Refused` - вызов заменён вызовом из другого потока либо родитель ответил концом
    ///   перечисления;
    /// - `NoSpace` - у родителя не хватило ресурса на результат;
    /// - `Ipc` - другой отказ транспорта.
    pub fn next(&self, wait: Timeout) -> Result<Found, StartupError> {
        answered(self.requirement, next(&self.watch, wait))?.ok_or(StartupError {
            requirement: self.requirement,
            failure: StartupFailure::Refused,
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
