use core::marker::PhantomData;

use component_abi::{STARTUP_REQUEST_TIMEOUT, SessionError};
use kernel_objects::{Session, kind};
use syscall::Handle;

use crate::{
    StartupError, StartupFailure,
    declare::{request_session, start_parent},
    request::session,
};

/// Провайдеры требования `all`. Стартовый handle принадлежит процессу до его завершения.
pub struct Providers<P> {
    parent: Handle<kind::Port>,
    requirement: &'static str,
    protocol: PhantomData<P>,
}

/// Сессии провайдеров по порядку списка. Отказ провайдера выдаётся элементом перебора.
pub struct ProviderSessions<'a, P> {
    providers: &'a Providers<P>,
    next: Option<u16>,
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

    /// Запрашивает сессию провайдера с индексом `index`. Каждый запрос ждёт ответ не дольше
    /// `STARTUP_REQUEST_TIMEOUT` и повторяется при `IpcError::PeerClosed`.
    pub fn session(&self, index: u16) -> Result<Session<P>, StartupError> {
        let parent = start_parent(self.parent).with_wait(STARTUP_REQUEST_TIMEOUT);

        request_session(self.requirement, || {
            session::<P>(&parent, self.requirement, index)
        })
    }

    pub fn iter(&self) -> ProviderSessions<'_, P> {
        ProviderSessions {
            providers: self,
            next: Some(0),
        }
    }
}

impl<'a, P: ipc::Protocol> IntoIterator for &'a Providers<P> {
    type Item = Result<Session<P>, StartupError>;
    type IntoIter = ProviderSessions<'a, P>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<P: ipc::Protocol> Iterator for ProviderSessions<'_, P> {
    type Item = Result<Session<P>, StartupError>;

    fn next(&mut self) -> Option<Self::Item> {
        next_provider(&mut self.next, |index| self.providers.session(index))
    }
}

fn next_provider<T>(
    next: &mut Option<u16>,
    request: impl FnOnce(u16) -> Result<T, StartupError>,
) -> Option<Result<T, StartupError>> {
    let index = (*next)?;
    let result = request(index);

    if matches!(
        result,
        Err(StartupError {
            failure: StartupFailure::Session(SessionError::Unbound),
            ..
        })
    ) {
        *next = None;
        return None;
    }
    *next = index.checked_add(1);

    Some(result)
}

#[cfg(test)]
mod tests {
    use super::{SessionError, StartupError, StartupFailure, next_provider};

    #[test]
    fn errors_do_not_end_iteration_but_unbound_does() {
        for failure in [
            StartupFailure::Session(SessionError::Unavailable),
            StartupFailure::Ipc(crate::IpcError::Timeout),
        ] {
            let mut next = Some(0);
            let error = StartupError {
                requirement: "clocks",
                failure,
            };

            assert_eq!(
                next_provider::<u16>(&mut next, |index| {
                    assert_eq!(index, 0);
                    Err(error)
                }),
                Some(Err(error))
            );
            assert_eq!(next_provider(&mut next, Ok), Some(Ok(1)));
            assert_eq!(
                next_provider::<u16>(&mut next, |index| {
                    assert_eq!(index, 2);
                    Err(StartupError {
                        requirement: "clocks",
                        failure: StartupFailure::Session(SessionError::Unbound),
                    })
                }),
                None
            );
            assert_eq!(
                next_provider::<u16>(&mut next, |_| panic!("finished iterator must not request")),
                None
            );
        }
    }
}
