use core::marker::PhantomData;

use component_abi::{Attributes, STARTUP_REQUEST_TIMEOUT, SessionError, component_startup};
use ipc::{CallError, Timeout};
use kernel_objects::{PortTransport, Session, kind};
use syscall::Handle;

use crate::{
    StartupError, StartupFailure,
    declare::{answered, request_session, start_parent},
    query::Query,
    request::{Found, all, next, open, watch},
};

type Enumerate = fn(
    &component_startup::Client<PortTransport>,
    &str,
    Attributes<'_>,
) -> Result<Session<component_startup::Protocol>, CallError<SessionError>>;

/// Провайдеры требования `all`. Стартовый handle принадлежит процессу до его завершения.
pub struct Providers<P> {
    parent: Handle<kind::Port>,
    requirement: &'static str,
    protocol: PhantomData<P>,
}

/// Результаты поиска по наборам запроса: наборы по очереди, результаты набора - в порядке выдачи
/// родителем. Отказ, кроме `NoSpace`, выдаётся последним элементом перебора. После `NoSpace`
/// следующий элемент повторяет запрос.
pub struct ProviderSessions<Q> {
    parent: Handle<kind::Port>,
    requirement: &'static str,
    enumerate: Enumerate,
    query: Q,
    step: usize,
    stage: Stage,
}

enum Stage {
    Enumerating(Session<component_startup::Protocol>),
    NextSet,
    Finished,
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

    /// Ищет экспорты протокола `P`, совпавшие с наборами `query`. Первый набор запрашивается
    /// вызовом, следующий - когда перебор исчерпал результаты предыдущего. Результаты набора
    /// фиксируются на момент его запроса, и новый вызов повторяет поиск.
    pub fn all<Q: Query>(&self, query: Q) -> Result<ProviderSessions<Q>, StartupError> {
        let mut sessions = ProviderSessions {
            parent: self.parent,
            requirement: self.requirement,
            enumerate: all::<P>,
            query,
            step: 0,
            stage: Stage::Finished,
        };
        sessions.stage = sessions.enumerate_next()?;
        Ok(sessions)
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

impl<Q: Query> ProviderSessions<Q> {
    fn enumerate_next(&mut self) -> Result<Stage, StartupError> {
        let Some(attributes) = self.query.attributes(self.step) else {
            return Ok(Stage::Finished);
        };

        let parent = start_parent(self.parent).with_wait(STARTUP_REQUEST_TIMEOUT);
        let enumeration = answered(
            self.requirement,
            (self.enumerate)(&parent, self.requirement, attributes),
        )?;
        self.step = self.step.saturating_add(1);
        Ok(Stage::Enumerating(enumeration))
    }
}

impl<Q: Query> Iterator for ProviderSessions<Q> {
    type Item = Result<Found, StartupError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let failed = match &self.stage {
                Stage::Enumerating(enumeration) => {
                    match answered(self.requirement, next(enumeration, STARTUP_REQUEST_TIMEOUT)) {
                        Ok(Some(found)) => return Some(Ok(found)),
                        // Исчерпанное перечисление закрывается до запроса следующего набора и
                        // не держит место в пределе поисков владельца.
                        Ok(None) => {
                            self.stage = Stage::NextSet;
                            continue;
                        }
                        Err(error) => error,
                    }
                }
                Stage::NextSet => match self.enumerate_next() {
                    Ok(stage) => {
                        self.stage = stage;
                        continue;
                    }
                    Err(error) => error,
                },
                Stage::Finished => return None,
            };

            if failed.failure != StartupFailure::NoSpace {
                self.stage = Stage::Finished;
            }
            return Some(Err(failed));
        }
    }
}
