//! Приём одноразового стартового кадра процесса.

use ipc::wire::IpcError;
use syscall::Handle;

use crate::{OwnedHandle, PortTransport};

/// Принимает один кадр startup-протокола и закрывает стартовый порт.
///
/// `dispatch` — сгенерированный IPC-диспетчер, сервисом которого служит
/// `Option<T>`. `None` означает, что обработанный кадр не установил аргументы.
pub fn receive_startup<T>(
    handle: Handle,
    dispatch: impl FnOnce(&mut Option<T>, &PortTransport) -> Result<(), IpcError>,
) -> Result<Option<T>, IpcError> {
    let handle = OwnedHandle::from_handle(handle);
    let transport = PortTransport::server(handle.borrow());
    let mut startup = None;
    dispatch(&mut startup, &transport)?;
    Ok(startup)
}
