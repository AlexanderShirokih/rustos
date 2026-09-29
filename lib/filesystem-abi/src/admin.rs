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
    use std::vec::Vec;

    use ipc::wire::Session;
    use ipc_test::{MockHandle, MockReply, MockResources};
    use syscall::kind;

    use super::{FileSystemError, namespace_admin};
    use crate::file_system;

    type FileSystemSession = Session<file_system::Protocol, MockHandle<kind::Port>>;

    const SESSION: FileSystemSession = Session::from_handle(MockHandle::new(0x41));

    #[derive(Default)]
    struct RouterSink {
        mounted: Vec<u32>,
    }

    impl namespace_admin::Service<MockResources> for RouterSink {
        fn mount(
            &mut self,
            _frame: &MockResources,
            filesystem: FileSystemSession,
            reply: namespace_admin::mount::Reply<MockReply>,
        ) {
            self.mounted.push(filesystem.into_handle().raw());
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
        assert_eq!(sink.mounted, [SESSION.into_handle().raw()]);
    }
}
