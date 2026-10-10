//! Открытие слота транспорта на шине.

use core::time::Duration;

use ipc::CallError;
use kernel_objects::{Session, SessionTransport};
use virtio::DeviceType;
use virtio_mmio_abi::{VirtioMmioError, virtio_mmio, virtio_slot};

const BUSY_ATTEMPTS: usize = 10;
const BUSY_PAUSE: Duration = Duration::from_millis(5);

/// Открывает слот свободного транспорта с устройством `device`.
///
/// Шина освобождает слот после того, как обработает уход прошлого клиента, и до этого
/// отвечает `Busy`. Функция повторяет `open` на `Busy` несколько раз с паузой и возвращает
/// последний ответ шины.
pub fn open_slot(
    bus: &virtio_mmio::Client<SessionTransport>,
    device: DeviceType,
) -> Result<Session<virtio_slot::Protocol>, CallError<VirtioMmioError>> {
    let mut attempts = BUSY_ATTEMPTS;
    loop {
        match bus.open(device as u32) {
            Err(CallError::Remote(VirtioMmioError::Busy)) if attempts > 1 => {
                attempts -= 1;
                runtime::sleep(BUSY_PAUSE);
            }
            opened => return opened.map(Session::from_session),
        }
    }
}
