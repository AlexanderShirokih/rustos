//! Обслуживание экспорта провайдером и создание портов сессий.

use core::{
    num::NonZeroU64,
    sync::atomic::{AtomicU64, Ordering},
};

use component_abi::{AcceptError, SessionError, component_startup};
use ipc::CallError;
use kernel_objects::{Error, Port, PortTransport};
use syscall::Rights;

use crate::request::accept;

/// Порт сессии из [`mint_session`] и его значок.
#[derive(Debug)]
pub struct MintedSession {
    pub badge: NonZeroU64,
    pub port: Port,
}

/// Отвечает на заявки импортёров на экспорт протокола `P` до первого отказа `accept`. `open`
/// строит порт сессии либо отказывает. Возвращает отказ `accept`.
pub fn serve_export<P: ipc::Protocol>(
    parent: &component_startup::Client<PortTransport>,
    mut open: impl FnMut() -> Result<Port, SessionError>,
) -> CallError<AcceptError> {
    loop {
        match accept::<P>(parent) {
            Ok(request) => {
                let session = open();
                let _ = request.reply.send(session);
            }
            Err(error) => return error,
        }
    }
}

/// Обработчик [`serve_export`], выдающий порт `export` под новым значком.
pub fn whole_export(export: &Port) -> impl FnMut() -> Result<Port, SessionError> + '_ {
    let next_badge = AtomicU64::new(0);

    move || {
        mint_session(export, &next_badge)
            .map(|minted| minted.port)
            .map_err(|_| SessionError::NoSpace)
    }
}

/// Создаёт клиентскую capability порта `export` с правами `WRITE | TRANSFER` под новым значком
/// из `next_badge`. Значок не повторяется и после отказа.
pub fn mint_session(export: &Port, next_badge: &AtomicU64) -> Result<MintedSession, Error> {
    let badge = NonZeroU64::MIN.saturating_add(next_badge.fetch_add(1, Ordering::Relaxed));
    let port = export.duplicate(Rights::WRITE | Rights::TRANSFER, Some(badge))?;

    Ok(MintedSession { badge, port })
}
