use core::fmt;

use component_abi::{Attributes, SessionError, component_startup};
use ipc::wire::IpcError;
use kernel_objects::{PortTransport, Session, kind};
use syscall::Handle;

use crate::request::session;

/// Требование, запрос которого завершился отказом, и причина отказа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartupError {
    pub requirement: &'static str,
    pub failure: StartupFailure,
}

/// Причина, по которой `request` не получил требование.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupFailure {
    /// Отказ в сессии требования.
    Session(component_abi::SessionError),
    /// Отказ транспорта либо истечение [`STARTUP_REQUEST_TIMEOUT`](crate::STARTUP_REQUEST_TIMEOUT).
    Ipc(IpcError),
    /// Стартового handle нет: его уже выдали, либо процесс запущен без него. Поле
    /// `requirement` отказа пустое.
    NoStartHandle,
}

/// Требования компонента, объявленные через `component_manifest!`.
/// `request` получает их по стартовой сессии родителя.
pub trait Requirements: Sized {
    fn request(parent: Handle<kind::Port>) -> Result<Self, StartupError>;
}

impl fmt::Display for StartupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            requirement,
            failure,
        } = self;
        write!(
            formatter,
            "startup error: requirement {requirement:?}: {failure:?}"
        )
    }
}

impl core::error::Error for StartupError {}

/// Объявляет манифест компонента: требования и экспортируемые протоколы.
///
/// Раскрытие даёт структуру с полем `parent` и полем на каждое требование, реализацию
/// [`Requirements`] и запись манифеста в секции `.component.manifest`. Протокол назван типом,
/// который порождает [`ipc::protocol!`].
///
/// `session(P)` даёт поле `Option<Session<P>>`, `all(P)` - [`Providers<P>`](crate::Providers).
/// Функция `request` запрашивает `session` без атрибутов, принимает `Unbound` как `None` и не
/// запрашивает `all`.
///
/// Экспорт `P => [{ compatible = "a" }, { compatible = "b", role = "c" }]` объявляет фильтры.
/// Экспорт совпадает с запросом, если все пары одного из фильтров есть среди атрибутов
/// запроса. Экспорт без фильтров совпадает только с запросом без атрибутов.
///
/// ```no_run
/// use component_startup::{component_manifest, Requirements};
///
/// ipc::protocol! {
///     mod rtc {
///         name = "Rtc";
///     }
/// }
///
/// ipc::protocol! {
///     mod alarm {
///         name = "Alarm";
///     }
/// }
///
/// component_manifest! {
///     struct AlarmStartup {
///         requirements: {
///             rtc: session(rtc::Protocol),
///         },
///         exports: [alarm::Protocol],
///     }
/// }
///
/// fn main() {}
/// ```
#[macro_export]
macro_rules! component_manifest {
    (exports: [$($export:path $(=> [$($filter:tt)*])?),* $(,)?] $(,)?) => {
        $crate::component_abi::__component_manifest_record! {
            exports = [$($export $(=> [$($filter)*])?),*]
            requirements {}
        }
    };
    (
        $(#[$attribute:meta])*
        $visibility:vis struct $name:ident {
            $(requirements: { $($requirement:ident: $kind:ident $(($($parameter:tt)*))?),* $(,)? } $(,)?)?
            $(exports: [$($export:path $(=> [$($filter:tt)*])?),* $(,)?] $(,)?)?
        }
    ) => {
        $(#[$attribute])*
        $visibility struct $name {
            /// Клиент стартовой сессии без срока ожидания.
            pub parent: $crate::component_startup::Client<$crate::PortTransport>,
            $($(pub $requirement: $crate::component_manifest!(@field $kind $(($($parameter)*))?),)*)?
        }

        impl $crate::Requirements for $name {
            /// Запрашивает у родителя требования в порядке объявления.
            fn request(
                parent: $crate::Handle<$crate::kind::Port>,
            ) -> ::core::result::Result<Self, $crate::StartupError> {
                let requests =
                    $crate::start_parent(parent).with_wait($crate::STARTUP_REQUEST_TIMEOUT);
                $($(
                    let $requirement = $crate::component_manifest!(
                        @request parent requests $requirement $kind $(($($parameter)*))?
                    )?;
                )*)?

                ::core::result::Result::Ok(Self {
                    parent: $crate::start_parent(parent),
                    $($($requirement,)*)?
                })
            }
        }

        $crate::component_abi::__component_manifest_record! {
            exports = [$($($export $(=> [$($filter)*])?),*)?]
            requirements { $($($requirement: $kind $(($($parameter)*))?),*)? }
        }
    };
    (@field session($protocol:path)) => { ::core::option::Option<$crate::Session<$protocol>> };
    (@field all($protocol:path)) => { $crate::Providers<$protocol> };
    (@request $parent:ident $requests:ident $requirement:ident session($protocol:path)) => {
        $crate::start_session::<$protocol>(&$requests, stringify!($requirement))
    };
    (@request $parent:ident $requests:ident $requirement:ident all($protocol:path)) => {
        ::core::result::Result::<_, $crate::StartupError>::Ok(
            $crate::Providers::<$protocol>::new($parent, stringify!($requirement))
        )
    };
}

#[must_use]
pub fn start_parent(parent: Handle<kind::Port>) -> component_startup::Client<PortTransport> {
    component_startup::Client::new(PortTransport::new(parent))
}

#[doc(hidden)]
pub fn start_session<P: ipc::Protocol>(
    requests: &component_startup::Client<PortTransport>,
    requirement: &'static str,
) -> Result<Option<Session<P>>, StartupError> {
    match request_session(requirement, || {
        session::<P>(requests, requirement, Attributes::empty())
    }) {
        Err(StartupError {
            failure: StartupFailure::Session(SessionError::Unbound),
            ..
        }) => Ok(None),
        result => result.map(Some),
    }
}

pub(crate) fn request_session<T>(
    requirement: &'static str,
    mut request: impl FnMut() -> Result<Result<T, SessionError>, IpcError>,
) -> Result<T, StartupError> {
    loop {
        match request() {
            Err(IpcError::PeerClosed) => {}
            answer => return answered(requirement, answer),
        }
    }
}

pub(crate) fn answered<T>(
    requirement: &'static str,
    answer: Result<Result<T, SessionError>, IpcError>,
) -> Result<T, StartupError> {
    answer
        .map_err(|error| failed(requirement, StartupFailure::Ipc(error)))?
        .map_err(|error| failed(requirement, StartupFailure::Session(error)))
}

const fn failed(requirement: &'static str, failure: StartupFailure) -> StartupError {
    StartupError {
        requirement,
        failure,
    }
}

#[cfg(test)]
mod tests {
    use component_abi::SessionError;
    use ipc::wire::IpcError;

    use super::{StartupError, StartupFailure, request_session};

    #[test]
    fn only_peer_closed_retries_a_session_request() {
        let mut calls = 0;
        let result = request_session("clocks", || {
            calls += 1;
            if calls == 1 {
                Err(IpcError::PeerClosed)
            } else {
                Ok(Ok(42))
            }
        });
        assert_eq!(result, Ok(42));
        assert_eq!(calls, 2);
        let mut calls = 0;
        let result = request_session::<u16>("clocks", || {
            calls += 1;
            Ok(Err(SessionError::Unbound))
        });
        assert_eq!(
            result,
            Err(StartupError {
                requirement: "clocks",
                failure: StartupFailure::Session(SessionError::Unbound)
            })
        );
        assert_eq!(calls, 1);
    }
}
