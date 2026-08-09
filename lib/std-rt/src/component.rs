//! Стартовый набор capability компонента композиции.
//!
//! Набор приходит по одноразовому порту, серверную сторону которого ядро кладёт в `x0` точки
//! входа. Лог-сессию из набора берёт `std` под `stdout` и `stderr`, остальное лежит здесь до
//! запроса программы.

use component_abi::{StartedComponent, receive_startup_transaction};
use syscall::Handle;

use crate::{PortTransport, lock::Lock, log, process::take_bootstrap, svc};

static STARTED: Lock<Option<StartedComponent>> = Lock::new(None);

/// Принимает стартовый набор и оставляет от него лог-сессию.
///
/// Вызывается точкой входа программы один раз до `main`. Процесс вне композиции порта в `x0`
/// не получает, и вызов сразу возвращается.
pub fn adopt_startup() {
    let Some(bootstrap) = take_bootstrap() else {
        return;
    };
    let started = receive_startup_transaction(&PortTransport::server(bootstrap));
    // Порт одноразовый: набор пришёл целиком либо не сложился, и второй попытки не будет.
    svc::handle_close(bootstrap);
    let Ok(Some(started)) = started else {
        return;
    };
    log::install(Handle::from_nonzero(started.log.raw()));
    *STARTED.lock() = Some(started);
}

/// Стартовый набор процесса, если композиция его выдала.
#[must_use]
pub fn startup() -> Option<StartedComponent> {
    *STARTED.lock()
}
