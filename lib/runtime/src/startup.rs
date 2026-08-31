//! Приём одноразового стартового кадра процесса.

use ipc::wire::IpcError;
use syscall::Handle;

use crate::{PortTransport, svc::handle_close};

/// Принимает один кадр startup-протокола и закрывает стартовый порт.
///
/// `dispatch` — сгенерированный IPC-диспетчер, сервисом которого служит
/// `Option<T>`. `None` означает, что обработанный кадр не установил аргументы.
pub fn receive_startup<T>(
    handle: Handle,
    dispatch: impl FnOnce(&mut Option<T>, &PortTransport) -> Result<(), IpcError>,
) -> Result<Option<T>, IpcError> {
    let transport = PortTransport::server(handle);
    let mut startup = None;
    let received = dispatch(&mut startup, &transport);
    // Порт одноразовый: кадр пришёл либо не сложился, и второй попытки по нему не будет.
    handle_close(handle);
    received?;
    Ok(startup)
}
