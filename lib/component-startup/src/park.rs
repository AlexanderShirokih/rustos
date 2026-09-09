//! Ожидание компонента, которому обслуживать нечего.

use ipc::{
    Timeout, Transport,
    wire::{MESSAGE_INLINE_MAX, MESSAGE_MAX_HANDLES},
};
use kernel_objects::{Port, PortTransport, ReceivedFrame};

/// Ждёт на startup-порту до конца процесса и закрывает capability входящих кадров.
///
/// Компонент без экспортов и без своего цикла заканчивает этим вызовом. Процесс
/// действует, пока его не снимет supervisor либо ядро. Входящие кадры закрываются
/// без ответа.
#[allow(clippy::needless_pass_by_value)]
pub fn park(startup: Port) -> ! {
    let port = PortTransport::server(&startup);
    loop {
        if port.wait_readable(Timeout::INFINITE).is_err() {
            continue;
        }
        let mut bytes = [0_u8; MESSAGE_INLINE_MAX];
        let mut handles = [0_u32; MESSAGE_MAX_HANDLES];
        let _ = port.read_message(&mut bytes, &mut handles);
        drop(ReceivedFrame::take(&port));
    }
}
