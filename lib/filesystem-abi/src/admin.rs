//! Контракт администратора маршрутизатора имён. Порт контракта отделён от порта `Namespace`.

use crate::error::FileSystemError;

ipc::protocol! {
    /// Сессия администратора монтирует одну файловую систему под префиксом, равным её области.
    pub mod namespace_admin {
        name = "NamespaceAdmin";

        /// Монтирует `filesystem` под префиксом области сессии на время вызова. `Ok(())` приходит
        /// после снятия монтирования: закрытия сессии, отказа `PeerClosed` на запрос к порту
        /// файловой системы либо монтирования другой сессии под тем же префиксом.
        call(defer) fn mount(
            own filesystem: ipc::wire::Session<crate::file_system::Protocol>,
        ) -> Result<(), FileSystemError>;
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU32;
    use std::vec::Vec;

    use ipc::wire::{Cap, Session};
    use ipc_test::{MockReply, MockResources};
    use syscall::Handle;

    use super::{FileSystemError, namespace_admin};
    use crate::file_system;

    const SESSION: Session<file_system::Protocol> = match NonZeroU32::new(0x41) {
        Some(id) => Session::from_handle(Cap::from_handle(Handle::from_abi(id))),
        None => panic!("non-zero"),
    };

    #[derive(Default)]
    struct RouterSink {
        mounted: Vec<u32>,
    }

    impl namespace_admin::Service<MockResources> for RouterSink {
        fn mount(
            &mut self,
            _frame: &MockResources,
            filesystem: Session<file_system::Protocol>,
            reply: namespace_admin::mount::Reply<MockReply>,
        ) {
            self.mounted.push(filesystem.into_handle().raw().get());
            reply
                .send(Ok::<(), FileSystemError>(()))
                .expect("the caller waits");
        }
    }

    #[test]
    fn a_mount_carries_only_the_session_of_its_file_system() {
        let (client_end, server_end) = ipc_test::pair();
        let mut sink = RouterSink::default();
        std::thread::scope(|scope| {
            let server = scope.spawn(|| {
                server_end
                    .serve(|frame| namespace_admin::dispatch(&mut sink, frame))
                    .expect("dispatch ok");
            });

            let client = namespace_admin::Client::new(client_end);
            assert_eq!(client.mount(SESSION).expect("call"), Ok(()));
            server.join().expect("server joins");
        });
        assert_eq!(sink.mounted, [SESSION.into_handle().raw().get()]);
    }
}
