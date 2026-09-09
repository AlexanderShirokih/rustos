//! Процесс файловой системы: компонентные порты, сессии клиентов и цикл обслуживания. Крейт
//! держит всё, что у файловых систем общее. Конкретная сводится к построению своего бекенда.
//!
//! Портов у процесса два. Startup-порт устанавливает сессии по
//! `ComponentStartup::open_session`, порт экспорта обслуживает их и выдаёт производные по
//! `open_root`. Startup-порт обслуживает отдельный поток. Вызов rootkeeper не ждёт вызова,
//! который сервисный поток обслуживает прямо сейчас.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use core::sync::atomic::AtomicU64;

use component_abi::component_startup;
use filesystem::FileSystemBackend;
use filesystem_abi::file_system;
use ipc::{Transport as _, wire::IpcError};
use kernel_objects::{MemoryRegion, Port, ReadOnlyMapping, ReceivedFrame, Resource};
use runtime::{Mutex, PortTransport, Priority, install_log, log_line, spawn_owned};
use spin::Once;
use syscall::Timeout;

mod admin;
mod buffer;
mod error;
mod service;
mod store;

use admin::Establishing;
pub use error::{Error, Result};
use service::{FileSystemState, Frame, Serving};

const ADMIN_STACK: u64 = 0x4000;

static IMAGE: Once<ReadOnlyMapping> = Once::new();

struct Shared {
    state: Mutex<FileSystemState>,
    service: Port,
    startup: Port,
    next_badge: AtomicU64,
}

/// Поднимает файловую систему и обслуживает её до закрытия service-порта.
///
/// `startup` и `service` приходят компонентной стартовой транзакцией, `backing` - регион с
/// байтами дерева, `tag` - имя процесса в строках лога. `found` строит бекенд из `Some(bytes)`
/// отображённого региона либо из `None` для автономного бекенда.
pub fn serve<B>(
    log: Port,
    startup: Port,
    service: Port,
    backing: Option<MemoryRegion>,
    tag: &'static str,
    found: impl FnOnce(Option<&'static [u8]>) -> Result<B>,
) -> Result<()>
where
    B: FileSystemBackend + Send + 'static,
{
    install_log(log.into_raw(), tag);
    let bytes = match backing.as_ref() {
        Some(backing) => {
            let mapping = backing.map_full_read_only()?;
            Some(IMAGE.call_once(|| mapping).as_bytes())
        }
        None => None,
    };
    let backend = found(bytes)?;

    let shared: &'static Shared = Box::leak(Box::new(Shared {
        state: Mutex::new(FileSystemState::new(
            Box::new(backend),
            backing,
            Resource::self_resource()?,
        )),
        service,
        startup,
        next_badge: AtomicU64::new(0),
    }));
    // Поток startup-порта разделяет время жизни процесса и владеет собственным стеком.
    let _ = spawn_owned(move || serve_admin(shared), ADMIN_STACK, Priority::new(1))?;
    log_line("started");
    serve_sessions(shared);
    Ok(())
}

fn serve_sessions(shared: &Shared) {
    let transport = PortTransport::server(shared.service.borrow());
    loop {
        match transport.wait_readable(Timeout::INFINITE) {
            Ok(()) => {}
            // Уход значка кадром не сопровождается. Сессия ушедшей стороны убирается, а приём
            // продолжается.
            Err(IpcError::SenderGone) => {
                shared.state.lock().remove_session(transport.peer_badge());
                continue;
            }
            // Исчерпание относится к одному кадру, а бессрочный приём срока не имеет. Буфер
            // приёма в обоих случаях не содержит доставленного сообщения.
            Err(IpcError::ResourceExhausted | IpcError::Timeout) => continue,
            Err(IpcError::PeerClosed) => break,
            Err(_) => {
                log_line("service port failed");
                return;
            }
        }
        // Приём выше доставил кадр, и его capability вместе с незавершённым ответом
        // принадлежат владеющему кадру до конца обработки.
        let Some(mut received) = ReceivedFrame::take(&transport) else {
            continue;
        };
        let mut frame = Frame::default();
        let mut serving = Serving {
            state: shared.state.lock(),
            frame: &mut frame,
            received: &mut received,
            service: &shared.service,
            next_badge: &shared.next_badge,
        };
        let delivered = file_system::dispatch(&mut serving, &transport).is_ok();
        drop(serving);
        frame.end(delivered, &mut shared.state.lock());
    }
    log_line("service port closed");
}

fn serve_admin(shared: &Shared) -> u32 {
    let transport = PortTransport::server(shared.startup.borrow());
    loop {
        match transport.wait_readable(Timeout::INFINITE) {
            Ok(()) => {}
            // Покадровое исчерпание, пробуждение без кадра и уход значка потребителя цикл не
            // закрывают, потому что снятие компонента по уходу его держателя выполняет ядро.
            Err(IpcError::SenderGone | IpcError::ResourceExhausted | IpcError::Timeout) => continue,
            Err(IpcError::PeerClosed) => break,
            // Прочий отказ приёма оставляет процесс без установления сессий и уходит ненулевым
            // кодом потока, отличимым от закрытия порта.
            Err(_) => {
                log_line("startup port failed");
                return 1;
            }
        }
        let received = ReceivedFrame::take(&transport);
        let mut establishing = Establishing::new(shared);
        let delivered = component_startup::dispatch(&mut establishing, &transport).is_ok();
        establishing.end(delivered);
        drop(received);
    }
    log_line("startup port closed");
    0
}
