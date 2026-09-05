//! Приём одноразового стартового кадра процесса.

use ipc::{Response, wire::IpcError};
use syscall::Handle;
use syscall_user::Delivery;

use crate::{PortTransport, svc::handle_close};

/// Принимает один кадр startup-протокола и закрывает стартовый порт.
///
/// `dispatch` — сгенерированный IPC-диспетчер, сервисом которого служит
/// `Option<T>`. `None` означает, что обработанный кадр не установил аргументы.
/// Capability кадра принадлежат возвращённому [`Response`]: невостребованные
/// закрываются вместе с ним.
pub fn receive_startup<T>(
    handle: Handle,
    dispatch: impl FnOnce(&mut Option<T>, &PortTransport) -> Result<(), IpcError>,
) -> Result<Option<Response<T, Delivery>>, IpcError> {
    let transport = PortTransport::server(handle);
    let mut startup = None;
    let received = dispatch(&mut startup, &transport);
    let delivery = transport.take_delivery().unwrap_or_default();
    // Порт одноразовый: кадр пришёл либо не сложился, и второй попытки по нему не будет.
    handle_close(handle);
    received?;
    Ok(startup.map(|value| Response::new(value, delivery)))
}
