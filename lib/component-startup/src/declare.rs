use core::fmt;

use component_abi::{REQUIREMENT_NAME_MAX, ResourceError, component_startup};
use ipc::wire::{IpcError, Str};
use kernel_objects::{OwnedHandle, PortTransport, ReceivedHandle, Session, kind};
use syscall::{Handle, ObjectKind};

use crate::request::session;

/// Первое требование, которое `request` не получил, и причина отказа. `request` не
/// запрашивает требования после него.
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
    /// Отказ в линии прерывания, MMIO-окне либо регионе образа.
    Resource(ResourceError),
    /// Отказ транспорта либо истечение [`STARTUP_REQUEST_TIMEOUT`](crate::STARTUP_REQUEST_TIMEOUT).
    Ipc(IpcError),
    /// Стартового handle нет: его уже выдали, либо процесс запущен без него. Поле
    /// `requirement` отказа пустое.
    NoStartHandle,
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
/// Раскрытие даёт структуру с полем `parent`, полем на каждое требование и функцией `request`,
/// а также запись манифеста в секции `.component.manifest`. Протокол назван типом, который
/// порождает [`ipc::protocol!`].
///
/// ```no_run
/// use component_startup::component_manifest;
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
///             window: mmio(compatible = ["arm,pl031"]),
///         },
///         exports: [alarm::Protocol],
///     }
/// }
///
/// fn main() {}
/// ```
#[macro_export]
macro_rules! component_manifest {
    (
        $(#[$attribute:meta])*
        $visibility:vis struct $name:ident {
            $(requirements: { $($requirement:ident: $kind:ident $(($($parameter:tt)*))?),* $(,)? } $(,)?)?
            $(exports: [$($export:path),* $(,)?] $(,)?)?
        }
    ) => {
        $(#[$attribute])*
        $visibility struct $name {
            /// Клиент стартовой сессии без срока ожидания.
            pub parent: $crate::component_startup::Client<$crate::PortTransport>,
            $($(pub $requirement: $crate::component_manifest!(@field $kind $(($($parameter)*))?),)*)?
        }

        impl $name {
            /// Запрашивает у родителя требования в порядке объявления.
            $visibility fn request(
                parent: $crate::Handle<$crate::kind::Port>,
            ) -> ::core::result::Result<Self, $crate::StartupError> {
                let requests =
                    $crate::start_parent(parent).with_wait($crate::STARTUP_REQUEST_TIMEOUT);
                $($(
                    let $requirement = $crate::component_manifest!(
                        @request requests $requirement $kind $(($($parameter)*))?
                    )?;
                )*)?

                ::core::result::Result::Ok(Self {
                    parent: $crate::start_parent(parent),
                    $($($requirement,)*)?
                })
            }
        }

        $crate::component_abi::__component_manifest_record! {
            exports = [$($($export),*)?]
            requirements { $($($requirement: $kind $(($($parameter)*))?),*)? }
        }
    };
    (@field session($protocol:path)) => { $crate::Session<$protocol> };
    (@field irq $parameters:tt) => { $crate::IrqLine };
    (@field mmio $parameters:tt) => { $crate::MemoryRegion };
    (@field region) => { $crate::MemoryRegion };
    (@request $requests:ident $requirement:ident session($protocol:path)) => {
        $crate::start_session::<$protocol>(&$requests, stringify!($requirement))
    };
    (@request $requests:ident $requirement:ident irq $parameters:tt) => {
        $crate::start_resource(stringify!($requirement), |name| $requests.irq(name))
    };
    (@request $requests:ident $requirement:ident mmio $parameters:tt) => {
        $crate::start_resource(stringify!($requirement), |name| $requests.mmio(name))
    };
    (@request $requests:ident $requirement:ident region) => {
        $crate::start_resource(stringify!($requirement), |name| $requests.region(name))
    };
}

#[must_use]
pub fn start_parent(parent: Handle<kind::Port>) -> component_startup::Client<PortTransport> {
    component_startup::Client::new(PortTransport::new(parent))
}

/// Запрашивает сессию требования `requirement` и повторяет запрос на `IpcError::PeerClosed`.
pub fn start_session<P: ipc::Protocol>(
    requests: &component_startup::Client<PortTransport>,
    requirement: &'static str,
) -> Result<Session<P>, StartupError> {
    loop {
        match session::<P>(requests, requirement) {
            Ok(Ok(session)) => return Ok(session),
            Ok(Err(error)) => return Err(failed(requirement, StartupFailure::Session(error))),
            Err(IpcError::PeerClosed) => {}
            Err(error) => return Err(failed(requirement, StartupFailure::Ipc(error))),
        }
    }
}

pub fn start_resource<K: ObjectKind>(
    requirement: &'static str,
    request: impl FnOnce(
        Str<'static, REQUIREMENT_NAME_MAX>,
    ) -> Result<Result<ReceivedHandle<K>, ResourceError>, IpcError>,
) -> Result<OwnedHandle<K>, StartupError> {
    let name = Str::new(requirement).ok_or(failed(
        requirement,
        StartupFailure::Ipc(IpcError::InvalidValue),
    ))?;
    match request(name) {
        Ok(Ok(handle)) => Ok(OwnedHandle::from(handle)),
        Ok(Err(error)) => Err(failed(requirement, StartupFailure::Resource(error))),
        Err(error) => Err(failed(requirement, StartupFailure::Ipc(error))),
    }
}

const fn failed(requirement: &'static str, failure: StartupFailure) -> StartupError {
    StartupError {
        requirement,
        failure,
    }
}
