//! Обслуживание admin-порта: стартовый кадр с capability, которых не даёт `ProcessStart`, и
//! установление сессий. Порт отдан тому, кто поднял файловую систему, и клиентам недоступен.

use core::{num::NonZeroU64, sync::atomic::Ordering::Relaxed};

use filesystem_abi::{FileSystemAdminService, FileSystemError};
use ipc::wire::Cap;
use runtime::{OwnedHandle, log_line};
use syscall::Rights;

use crate::{
    Shared,
    error::no_space,
    service::{settle, vend},
};

pub(crate) struct StartResources {
    pub(crate) log: OwnedHandle,
    pub(crate) image: OwnedHandle,
}

#[derive(Default)]
pub(crate) struct Handshake {
    resources: Option<StartResources>,
}

pub(crate) struct Binding<'a> {
    shared: &'a Shared,
    vended: Option<OwnedHandle>,
}

impl Handshake {
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

    pub(crate) fn end(&mut self, delivered: bool) {
        settle(&mut self.vended, delivered);
    }
}

impl FileSystemAdminService for Binding<'_> {
    fn start(&mut self, log: Cap, image: Cap) {
        drop(OwnedHandle::adopt(log));
        drop(OwnedHandle::adopt(image));
        log_line("repeated start frame ignored");
    }

    fn bind(&mut self) -> Result<Cap, FileSystemError> {
        // Значения значков не переиспользуются: счётчик только растёт.
        let badge = NonZeroU64::MIN.saturating_add(self.shared.next_badge.fetch_add(1, Relaxed));
        let session = self
            .shared
            .service
            .duplicate(Rights::WRITE | Rights::TRANSFER, Some(badge))
            .map_err(no_space)?;
        // Сессия заводится до того, как её капа ушла клиенту: кадр по ней застанет её заведённой.
        self.shared.state.lock().open_session(badge.get())?;
        Ok(vend(&mut self.vended, session))
    }
}
