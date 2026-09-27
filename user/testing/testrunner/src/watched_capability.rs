//! Наблюдение за судьбой доставленной capability.
//!
//! Capability выводится из порта под своим значком, и уход её последнего держателя виден
//! отказом приёма на этом порту. Так закрытие проверяется без участия получателя.

use core::num::NonZeroU64;

use kernel_objects::{Error, Port};
use runtime::Timeout;
use syscall::{Rights, SyscallError};

const CLOSURE_TIMEOUT: Timeout = Timeout::from_secs(5);

const LIVENESS_TIMEOUT: Timeout = Timeout::from_millis(200);

pub(crate) struct Watched {
    port: Port,
    badge: NonZeroU64,
}

impl Watched {
    pub(crate) fn create(badge: NonZeroU64) -> Self {
        Self {
            port: Port::create().expect("watched port"),
            badge,
        }
    }

    pub(crate) fn deliver(&self) -> Port {
        self.port
            .duplicate(Rights::WRITE | Rights::TRANSFER, Some(self.badge))
            .expect("delivered capability minted")
    }

    pub(crate) fn assert_closed(&self) {
        kernel_tests::kassert!(matches!(
            self.port.recv(CLOSURE_TIMEOUT).err(),
            Some(Error::Syscall(SyscallError::SenderGone))
        ));
    }

    pub(crate) fn assert_alive(&self) {
        kernel_tests::kassert!(matches!(
            self.port.recv(LIVENESS_TIMEOUT).err(),
            Some(Error::Syscall(SyscallError::Timeout))
        ));
    }
}
