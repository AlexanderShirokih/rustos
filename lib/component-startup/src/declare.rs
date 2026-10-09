use core::fmt;

use component_abi::{SessionError, component_startup};
use ipc::{CallError, wire::IpcError};
use kernel_objects::{PortTransport, kind};
use syscall::Handle;

/// Требование, запрос которого завершился отказом, и причина отказа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartupError {
    pub requirement: &'static str,
    pub failure: StartupFailure,
}

/// Причина, по которой требование не получено.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupFailure {
    /// Ни один экспорт не совпал с запросом.
    Unbound,
    /// Требование не объявлено, запрос недопустим либо совпало несколько экспортов.
    Refused,
    /// Провайдер экспорта снят навсегда.
    Unavailable,
    /// Заявку вытеснили сверх предела либо у родителя не хватило ресурса.
    NoSpace,
    /// Отказ транспорта либо истечение [`STARTUP_REQUEST_TIMEOUT`](crate::STARTUP_REQUEST_TIMEOUT).
    Ipc(IpcError),
    /// Стартового handle нет: его уже выдали, либо процесс запущен без него. Поле
    /// `requirement` отказа пустое.
    NoStartHandle,
}

/// Требования компонента, построенные по стартовой сессии. Структура `component_manifest!`
/// собирает поля требований, handle стартовой сессии служит требованиями компонента без
/// структуры манифеста.
pub trait Requirements {
    /// Строит требования по стартовой сессии `parent` без запросов к родителю.
    fn from_parent(parent: Handle<kind::Port>) -> Self;
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

impl Requirements for Handle<kind::Port> {
    fn from_parent(parent: Handle<kind::Port>) -> Self {
        parent
    }
}

impl From<SessionError> for StartupFailure {
    fn from(error: SessionError) -> Self {
        match error {
            SessionError::Unbound => Self::Unbound,
            SessionError::Refused => Self::Refused,
            SessionError::Unavailable => Self::Unavailable,
            SessionError::NoSpace => Self::NoSpace,
        }
    }
}

/// Объявляет манифест компонента: требования и экспортируемые протоколы.
///
/// Раскрытие даёт структуру с полем `parent` и полем на каждое требование, реализацию
/// [`Requirements`] и запись манифеста в секции `.component.manifest`. Протокол назван типом,
/// который порождает [`ipc::protocol!`].
///
/// `session(P)` даёт поле [`Import<P>`](crate::Import), `all(P)` -
/// [`Providers<P>`](crate::Providers). Построение структуры не обращается к родителю, сессию
/// требования открывает вызов поля.
///
/// `features: ["arm,pl031"]` называет признаки машины. Экспорты компонента доступны поиску, когда
/// все его признаки опубликованы.
///
/// Экспорт `P => [{ kind = "a" }, { kind = "b", role = "c" }]` объявляет фильтры.
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
///         requirements: { rtc: session(rtc::Protocol) },
///         features: ["arm,pl031"],
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
            features []
        }
    };
    (
        $(#[$attribute:meta])*
        $visibility:vis struct $name:ident {
            $(requirements: { $($requirement:ident: $kind:ident($protocol:path)),* $(,)? } $(,)?)?
            $(features: [$($feature:literal),* $(,)?] $(,)?)?
            $(exports: [$($export:path $(=> [$($filter:tt)*])?),* $(,)?] $(,)?)?
        }
    ) => {
        $(#[$attribute])*
        $visibility struct $name {
            /// Клиент стартовой сессии без срока ожидания.
            pub parent: $crate::component_startup::Client<$crate::PortTransport>,
            $($(pub $requirement: $crate::component_manifest!(@field $kind($protocol)),)*)?
        }

        impl $crate::Requirements for $name {
            fn from_parent(parent: $crate::Handle<$crate::kind::Port>) -> Self {
                Self {
                    parent: $crate::start_parent(parent),
                    $($($requirement: $crate::component_manifest!(
                        @new parent $requirement $kind($protocol)
                    ),)*)?
                }
            }
        }

        $crate::component_abi::__component_manifest_record! {
            exports = [$($($export $(=> [$($filter)*])?),*)?]
            requirements { $($($requirement: $kind($protocol)),*)? }
            features [$($($feature),*)?]
        }
    };
    (@field session($protocol:path)) => { $crate::Import<$protocol> };
    (@field all($protocol:path)) => { $crate::Providers<$protocol> };
    (@new $parent:ident $requirement:ident session($protocol:path)) => {
        $crate::Import::<$protocol>::new($parent, stringify!($requirement))
    };
    (@new $parent:ident $requirement:ident all($protocol:path)) => {
        $crate::Providers::<$protocol>::new($parent, stringify!($requirement))
    };
}

#[must_use]
pub fn start_parent(parent: Handle<kind::Port>) -> component_startup::Client<PortTransport> {
    component_startup::Client::new(PortTransport::new(parent))
}

pub(crate) fn request_session<T>(
    requirement: &'static str,
    mut request: impl FnMut() -> Result<T, CallError<SessionError>>,
) -> Result<T, StartupError> {
    loop {
        match request() {
            Err(CallError::Ipc(IpcError::PeerClosed)) => {}
            answer => return answered(requirement, answer),
        }
    }
}

pub(crate) fn answered<T>(
    requirement: &'static str,
    answer: Result<T, CallError<SessionError>>,
) -> Result<T, StartupError> {
    answer.map_err(|error| StartupError {
        requirement,
        failure: error.fold(StartupFailure::Ipc, StartupFailure::from),
    })
}

#[cfg(test)]
mod tests {
    use component_abi::SessionError;
    use ipc::{CallError, wire::IpcError};

    use super::{StartupError, StartupFailure, request_session};

    #[test]
    fn only_peer_closed_retries_a_session_request() {
        let mut calls = 0;
        let result = request_session("clocks", || {
            calls += 1;
            if calls == 1 {
                Err(CallError::Ipc(IpcError::PeerClosed))
            } else {
                Ok(42)
            }
        });
        assert_eq!(result, Ok(42));
        assert_eq!(calls, 2);
        let mut calls = 0;
        let result = request_session::<u16>("clocks", || {
            calls += 1;
            Err(CallError::Remote(SessionError::Unbound))
        });
        assert_eq!(
            result,
            Err(StartupError {
                requirement: "clocks",
                failure: StartupFailure::Unbound
            })
        );
        assert_eq!(calls, 1);
    }
}
