//! Relay логирования rootkeeper: сток - bootstrap-капа ядра, которая процесс не
//! покидает, а подсистемам достаются только лог-сессии.

use alloc::{string::String, vec::Vec};
use core::num::NonZeroU64;

use kernel_objects::{OwnedHandle, Result};
use log_abi::{LOG_SOURCE_MAX, LogBuffer};
use log_relay::{KernelLogSink, LogRelayThread, SourceNames};
use runtime::Timeout;
use spin::Mutex;
use syscall::Handle;

/// Значок лог-сессии супервизора.
pub const SUPERVISOR_BADGE: NonZeroU64 = NonZeroU64::new(1).expect("badge is non-zero");

const SUPERVISOR_NAME: &str = "rootkeeper";

/// Значок лог-сессии компонента с номером `index` в плане.
#[must_use]
pub fn component_badge(index: u16) -> NonZeroU64 {
    // Слагаемые умещаются в `u64` с запасом, и сумма насыщения не достигает.
    SUPERVISOR_BADGE.saturating_add(1 + u64::from(index))
}

/// Номер записи плана, которой принадлежит значок `badge`.
#[must_use]
pub fn component_index(badge: NonZeroU64) -> Option<u16> {
    let index = badge.get().checked_sub(SUPERVISOR_BADGE.get() + 1)?;
    u16::try_from(index).ok()
}

static RELAY: Mutex<Option<LogRelayThread>> = Mutex::new(None);

static COMPONENT_NAMES: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Имена источников строк журнала композиции: значок лог-сессии - имя экземпляра, которому
/// она выдана.
pub struct ComponentNames;

impl SourceNames for ComponentNames {
    fn name(&self, badge: u64) -> Option<LogBuffer<LOG_SOURCE_MAX>> {
        if badge == SUPERVISOR_BADGE.get() {
            return Some(source(SUPERVISOR_NAME));
        }
        let index = usize::from(component_index(NonZeroU64::new(badge)?)?);
        COMPONENT_NAMES
            .lock()
            .get(index)
            .map(|name| source(name.as_str()))
    }
}

/// Создаёт лог-порт и его выделенный поток. `kernel` - bootstrap-капа ядра, которую
/// получает только сток relay.
pub fn start(kernel: Handle) -> Result<()> {
    let relay = LogRelayThread::start(ComponentNames, KernelLogSink::new(kernel))?;
    *RELAY.lock() = Some(relay);
    Ok(())
}

/// Называет источники строк журнала именами компонентов набора композиции в его порядке.
/// Транзакция расширения пополняет набор. Вызов повторяется на каждой его смене.
pub fn install_names<'a>(ids: impl Iterator<Item = &'a str>) {
    // Имена читает поток relay, переживающий и разбор плана, и откат транзакции. Таблица
    // держит копии и уходит вместе со снятыми компонентами.
    let names: Vec<String> = ids.map(String::from).collect();
    // Освобождение прежней таблицы не выполняется под локом: куча общая с потоком relay.
    let replaced = core::mem::replace(&mut *COMPONENT_NAMES.lock(), names);
    drop(replaced);
}

/// Минтит лог-сессию со значком `badge`.
#[must_use]
pub fn session(badge: NonZeroU64) -> Option<OwnedHandle> {
    RELAY.lock().as_ref()?.session(badge).ok()
}

/// Минтит лог-сессию подсистемы, раздающей логирование своим процессам.
#[must_use]
pub fn delegated_session(badge: NonZeroU64) -> Option<OwnedHandle> {
    RELAY.lock().as_ref()?.delegated_session(badge).ok()
}

/// Waitable handle потока пересылки.
#[must_use]
pub fn thread_handle() -> Option<Handle> {
    RELAY.lock().as_ref().map(LogRelayThread::handle)
}

/// Финальный exit-код завершённого потока пересылки.
#[must_use]
pub fn exit_code() -> Option<u32> {
    RELAY.lock().as_ref()?.exit_code().ok()
}

/// Закрывает лог-порт и ждёт завершения цикла пересылки.
///
/// Занятый лок оставляет цикл живым. Ожидание лока на пути завершения
/// превращает отказ в зависание.
pub fn shutdown(timeout: Timeout) {
    if let Some(mut held) = RELAY.try_lock()
        && let Some(relay) = held.take()
    {
        let _ = relay.shutdown(timeout);
    }
}

fn source(name: &str) -> LogBuffer<LOG_SOURCE_MAX> {
    let mut buffer = LogBuffer::new();
    buffer.push_str(name);
    buffer
}
