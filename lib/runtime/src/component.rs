//! Стартовый набор capability компонента композиции.
//!
//! Набор приходит по одноразовому порту, серверную сторону которого ядро кладёт в `x0` точки
//! входа. Лог-сессию из набора берёт `std` под `stdout` и `stderr`, остальное лежит здесь до
//! запроса программы.

use component_abi::{StartedComponent, receive_startup_transaction};
use syscall::Handle;

use crate::{
    log,
    process::{bootstrap_handle, clear_bootstrap},
    svc,
    svc::PortTransport,
    sync::Mutex,
};

static STARTED: Mutex<Option<StartedComponent>> = Mutex::new(None);

/// Принимает стартовый набор и оставляет от него лог-сессию.
///
/// Вызывается точкой входа программы один раз до `main`. Процесс вне композиции порта в `x0`
/// не получает, и вызов сразу возвращается. Стартовый хэндл, по которому набор не сложился,
/// остаётся процессу. По нему корень композиции обращается к ядру.
pub fn adopt_startup() {
    let Some(bootstrap) = bootstrap_handle() else {
        return;
    };
    let Ok(Some(started)) = receive_startup_transaction(&PortTransport::server(bootstrap)) else {
        return;
    };
    // Порт одноразовый: набор пришёл целиком, и второй попытки по нему не будет.
    clear_bootstrap();
    svc::handle_close(bootstrap);
    log::install(Handle::from_nonzero(started.log.raw()), "");
    *STARTED.lock() = Some(started);
}

/// Стартовый набор процесса, если композиция его выдала.
#[must_use]
pub fn startup() -> Option<StartedComponent> {
    *STARTED.lock()
}
