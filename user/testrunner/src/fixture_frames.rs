//! Запуск программы-фикстуры и отправка ей кадров по её стартовому порту.
//!
//! Порт одноразовый, отправитель уходит вместе с ним, и его уход виден фикстуре отказом
//! приёма. Кадр собирается вручную там, где сгенерированный клиент контракта нужного набора
//! полей и capability не даёт.

use alloc::vec::Vec;
use core::num::NonZeroU64;

use component_abi::component_startup;
use ipc::{
    Transport as _,
    wire::{Header, IpcError, MESSAGE_INLINE_MAX, MessageBuf},
};
use kernel_objects::{OwnedHandle, Process, Resource};
use managed_component::ManagedStartupError;
use runtime::{PortTransport, Timeout};
use userland_loader::{InitializedProcess, launch_entry_retained};

use crate::managed_startup::{image_mapping, image_program};

const DELIVERY_TIMEOUT: Timeout = Timeout::from_secs(1);

const STARTUP_LEASE_BADGE: NonZeroU64 = NonZeroU64::new(0x76).expect("badge is non-zero");

pub(crate) fn launch_entry(
    entry: &str,
    name: &str,
    send: impl FnOnce(PortTransport) -> Result<(), IpcError>,
) -> Process {
    let mapping = image_mapping();
    let program = image_program(&mapping, entry);
    let resource = Resource::self_resource().expect("self metering resource");

    let InitializedProcess { process, startup } = launch_entry_retained(
        &program,
        Some(name),
        &resource,
        STARTUP_LEASE_BADGE,
        move |startup| -> Result<(), ManagedStartupError> {
            let transport =
                PortTransport::client(startup.handle()).with_send_timeout(DELIVERY_TIMEOUT);
            send(transport).map_err(ManagedStartupError::from)
        },
    )
    .expect("startup frames delivered");

    // Транзакция одноразовая, и уход значка стартового порта виден компоненту отказом
    // приёма. Так обрывается транзакция, оставленная без обещанного слота.
    drop(startup);
    process
}

pub(crate) fn send_raw_frame(
    transport: &PortTransport,
    txid: u32,
    carried: Vec<OwnedHandle>,
) -> Result<(), IpcError> {
    let mut frame = MessageBuf::<MESSAGE_INLINE_MAX>::new();
    frame.write_header(&Header::new(component_startup::start::ORDINAL, txid, 0))?;
    frame.finish()?;

    let handles: Vec<u32> = carried.iter().map(|handle| handle.as_raw().raw()).collect();
    let sent = transport.write_message(frame.as_bytes(), &handles);
    if sent.is_ok() {
        // Кадр принят транспортом: закрывать его capability отправителю больше нечего.
        for handle in carried {
            let _ = handle.into_raw();
        }
    }
    sent
}
