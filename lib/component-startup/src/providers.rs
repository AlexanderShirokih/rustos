use core::marker::PhantomData;

use component_abi::{
    Attributes, STARTUP_REQUEST_TIMEOUT, SessionError, SessionRequirement, component_startup,
};
use executor::{PortChannel, Reactor};
use ipc::{CallError, Timeout};
use kernel_objects::{Port, Session, kind};
use syscall::Handle;

use crate::{
    StartupError, StartupFailure,
    declare::{answered, repeats},
    query::Query,
    request::{AsyncStartup, Found, next, open, watch},
};

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
    protocol: &'static str,
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

    /// Ищет экспорты протокола `P`, совпавшие с наборами `query`. Ответы родителя задача ждёт
    /// через `reactor`. Наборы запрашиваются по очереди, результаты набора фиксируются на
    /// момент запроса.
    pub async fn all<Q: Query>(
        &self,
        reactor: &Reactor,
        query: Q,
    ) -> Result<ProviderSessions<Q>, StartupError> {
        let mut sessions = ProviderSessions {
            parent: self.parent,
            requirement: self.requirement,
            protocol: P::ID,
            query,
            step: 0,
            stage: Stage::Finished,
        };

        sessions.stage = sessions.enumerate_next(reactor).await?;
        Ok(sessions)
    }

    /// Запрашивает наблюдение за экспортами протокола `P`, совпавшими с `attributes`. Ответ
    /// родителя задача ждёт через `reactor` не дольше `STARTUP_REQUEST_TIMEOUT`.
    pub async fn watch(
        &self,
        reactor: &Reactor,
        attributes: Attributes<'_>,
    ) -> Result<ProviderWatch, StartupError> {
        let parent = AsyncStartup::new(PortChannel::new(self.parent, reactor.clone()))
            .with_wait(STARTUP_REQUEST_TIMEOUT);

        let watch = answered(
            self.requirement,
            watch::<P>(&parent, self.requirement, attributes).await,
        )?;

        Ok(ProviderWatch {
            watch,
            requirement: self.requirement,
        })
    }

    /// Открывает сессию экспорта `found` через `reactor`. Каждый запрос ждёт не дольше
    /// `STARTUP_REQUEST_TIMEOUT` и повторяется при `IpcError::PeerClosed`.
    pub async fn open(&self, reactor: &Reactor, found: &Found) -> Result<Session<P>, StartupError> {
        loop {
            let answer = open::<P>(found, reactor, STARTUP_REQUEST_TIMEOUT).await;

            if !repeats(&answer) {
                return answered(self.requirement, answer);
            }
        }
    }

    /// Открывает через `reactor` сессию первого экспорта, совпавшего с наборами `query`.
    /// `None` - ни один набор не дал совпадений.
    pub async fn open_first<Q: Query>(
        &self,
        reactor: &Reactor,
        query: Q,
    ) -> Result<Option<Session<P>>, StartupError> {
        let Some(found) = self
            .all(reactor, query)
            .await?
            .next(reactor)
            .await
            .transpose()?
        else {
            return Ok(None);
        };

        self.open(reactor, &found).await.map(Some)
    }
}

impl ProviderWatch {
    /// Ждёт не дольше `wait` экспорт, ещё не выданный этим наблюдением. Ответ родителя
    /// задача ждёт через `reactor`.
    ///
    /// Отказы:
    /// - `Ipc(IpcError::Timeout)` - срок истёк, экспорт остаётся для следующего вызова;
    /// - `Refused` - вызов заменён либо родитель ответил концом перечисления;
    /// - `NoSpace` - у родителя не хватило ресурса на результат;
    /// - `Ipc` - другой отказ транспорта.
    pub async fn next(&self, reactor: &Reactor, wait: Timeout) -> Result<Found, StartupError> {
        self.found(next(&self.watch, reactor, wait).await)
    }

    fn found(
        &self,
        answer: Result<Option<Found>, CallError<SessionError>>,
    ) -> Result<Found, StartupError> {
        answered(self.requirement, answer)?.ok_or(StartupError {
            requirement: self.requirement,
            failure: StartupFailure::Refused,
        })
    }
}

impl<Q: Query> ProviderSessions<Q> {
    /// Забирает следующий результат перечисления через `reactor`.
    pub async fn next(&mut self, reactor: &Reactor) -> Option<Result<Found, StartupError>> {
        loop {
            let failed = match &self.stage {
                Stage::Enumerating(enumeration) => {
                    match answered(
                        self.requirement,
                        next(enumeration, reactor, STARTUP_REQUEST_TIMEOUT).await,
                    ) {
                        Ok(Some(found)) => return Some(Ok(found)),
                        Ok(None) => {
                            self.stage = Stage::NextSet;
                            continue;
                        }

                        Err(error) => error,
                    }
                }
                Stage::NextSet => match self.enumerate_next(reactor).await {
                    Ok(stage) => {
                        self.stage = stage;
                        continue;
                    }

                    Err(error) => error,
                },
                Stage::Finished => return None,
            };

            return Some(Err(self.failed(failed)));
        }
    }

    async fn enumerate_next(&mut self, reactor: &Reactor) -> Result<Stage, StartupError> {
        let Some(attributes) = self.query.attributes(self.step) else {
            return Ok(Stage::Finished);
        };

        let parent = AsyncStartup::new(PortChannel::new(self.parent, reactor.clone()))
            .with_wait(STARTUP_REQUEST_TIMEOUT);

        let enumeration = answered(
            self.requirement,
            parent
                .all(
                    SessionRequirement {
                        name: self.requirement,
                        protocol: self.protocol,
                    },
                    attributes.as_bytes(),
                )
                .await,
        )?;

        self.step = self.step.saturating_add(1);
        Ok(Stage::Enumerating(Session::from_handle(Port::from(
            enumeration,
        ))))
    }

    fn failed(&mut self, failed: StartupError) -> StartupError {
        if failed.failure != StartupFailure::NoSpace {
            self.stage = Stage::Finished;
        }

        failed
    }
}
