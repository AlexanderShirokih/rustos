//! Открытие слота транспорта на шине.

use executor::PortChannel;
use ipc::CallError;
use kernel_objects::{Session, SessionTransport};
use runtime::Timeout;
use virtio::DeviceType;
use virtio_mmio_abi::{VirtioMmioError, virtio_mmio, virtio_slot};

const BUSY_ATTEMPTS: usize = 10;
const BUSY_PAUSE: Timeout = Timeout::from_millis(5);

/// Открывает слот свободного транспорта с устройством `device`.
///
/// Шина освобождает слот после того, как обработает уход прошлого клиента, и до этого
/// отвечает `Busy`. Функция повторяет `open` на `Busy` несколько раз с паузой и возвращает
/// последний ответ шины.
pub async fn open_slot(
    bus: &virtio_mmio::Client<PortChannel<SessionTransport>>,
    device: DeviceType,
) -> Result<Session<virtio_slot::Protocol>, CallError<VirtioMmioError>> {
    let mut attempts = BUSY_ATTEMPTS;
    loop {
        match bus.open(device as u32).await {
            Err(CallError::Remote(VirtioMmioError::Busy)) if attempts > 1 => {
                attempts -= 1;
                executor::sleep(BUSY_PAUSE).await;
            }
            opened => return opened.map(Session::from_session),
        }
    }
}
