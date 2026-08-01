//! Обслуживание admin-порта: стартовый кадр с capability, которых не даёт `ProcessStart`, и
//! установление сессий. Порт отдан тому, кто поднял файловую систему, и клиентам недоступен.

use core::sync::atomic::Ordering::Relaxed;

use filesystem_abi::{FileSystemAdminService, FileSystemError};
use ipc::wire::Cap;
use runtime::{OwnedHandle, log_line};
use syscall::Rights;

use crate::{
    Shared,
    error::no_space,
    service::{settle, vend},
};

/// Capability стартового кадра.
pub(crate) struct StartResources {
    /// Канал логирования в klog.
    pub(crate) log: OwnedHandle,
    /// Регион userland-образа.
    pub(crate) image: OwnedHandle,
}

/// Сторона admin-порта до подъёма сервера: она только принимает стартовый кадр.
#[derive(Default)]
pub(crate) struct Handshake {
    resources: Option<StartResources>,
}

/// Сторона admin-порта поднятого сервера: она заводит сессии.
pub(crate) struct Binding<'a> {
    shared: &'a Shared,
    vended: Option<OwnedHandle>,
}

impl Handshake {
    /// Принятые capability. `None` - стартового кадра не было.
    pub(crate) fn take(self) -> Option<StartResources> {
        self.resources
    }
}

impl FileSystemAdminService for Handshake {
    fn start(&mut self, log: Cap, image: Cap) {
        self.resources = Some(StartResources {
            log: OwnedHandle::adopt(log),
            image: OwnedHandle::adopt(image),
        });
    }

    /// Сессии заводит поднятый сервер: до стартового кадра ни сервисного порта, ни таблицы сессий
    /// ещё нет.
    fn bind(&mut self) -> Result<Cap, FileSystemError> {
        Err(FileSystemError::NoSession)
    }
}

impl<'a> Binding<'a> {
    pub(crate) fn new(shared: &'a Shared) -> Self {
        Self {
            shared,
            vended: None,
        }
    }

    /// Закрывает кадр: выданная capability остаётся за вызывателем только при доставленном ответе.
    pub(crate) fn end(&mut self, delivered: bool) {
        settle(&mut self.vended, delivered);
    }
}

impl FileSystemAdminService for Binding<'_> {
    /// Стартовый кадр сервер принимает один раз, до подъёма. Повторный несёт capability, которых
    /// сервер не ждёт: они закрываются вместе с кадром.
    fn start(&mut self, log: Cap, image: Cap) {
        drop(OwnedHandle::adopt(log));
        drop(OwnedHandle::adopt(image));
        log_line("repeated start frame ignored");
    }

    fn bind(&mut self) -> Result<Cap, FileSystemError> {
        // Значения значков не переиспользуются: счётчик только растёт, а нулевой значок означает
        // капу без сессии и наружу не уходит.
        let badge = self.shared.next_badge.fetch_add(1, Relaxed);
        let session = self
            .shared
            .service
            .duplicate(Rights::WRITE | Rights::TRANSFER, badge)
            .map_err(no_space)?;
        // Сессия заводится до того, как её капа ушла клиенту: кадр по ней застанет её заведённой.
        self.shared.state.lock().open_session(badge)?;
        Ok(vend(&mut self.vended, session))
    }
}
