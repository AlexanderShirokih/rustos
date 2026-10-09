//! Контракт монтирования файловых систем по абсолютному пути.

use crate::error::FileSystemError;

ipc::wire_enum! {
    /// Причина снятия монтирования.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum MountEnded {
        /// Клиент закрыл сессию монтирования.
        Closed = 1,
        /// Другая сессия заместила монтирование по тому же пути.
        Replaced = 2,
        /// Порт файловой системы закрыт.
        FileSystemGone = 3,
    }
}

ipc::protocol! {
    /// Сессия монтирования файловой системы.
    pub mod file_system_mount {
        name = "FileSystemMount";

        /// Монтирует `filesystem` по абсолютному `path` на время вызова. Ответ приходит
        /// после снятия монтирования: закрытия сессии, отказа `PeerClosed` на запрос к порту
        /// файловой системы либо монтирования другой сессии под тем же префиксом.
        call(defer) fn mount(
            path: &str,
            own filesystem: ipc::wire::Session<crate::file_system::Protocol>,
        ) -> Result<MountEnded, FileSystemError>;
    }
}

#[cfg(test)]
mod tests {
    use std::vec::Vec;

    use ipc::wire::Session;
    use ipc_test::{MockHandle, MockReply, MockResources};
    use syscall::kind;

    use super::{FileSystemError, MountEnded, file_system_mount};
    use crate::file_system;

    type FileSystemSession = Session<file_system::Protocol, MockHandle<kind::Port>>;

    const SESSION: FileSystemSession = Session::from_handle(MockHandle::new(0x41));

    #[derive(Default)]
    struct RouterSink {
        mounted: Vec<(std::string::String, u32)>,
    }

    impl file_system_mount::Service<MockResources> for RouterSink {
        fn mount(
            &mut self,
            _frame: &MockResources,
            path: &str,
            filesystem: FileSystemSession,
            reply: file_system_mount::mount::Reply<MockReply>,
        ) {
            self.mounted
                .push((path.into(), filesystem.into_handle().raw()));
            reply
                .send(Ok::<_, FileSystemError>(MountEnded::Replaced))
                .expect("the caller waits");
        }
    }

    #[test]
    fn a_mount_carries_its_path_and_file_system_capability() {
        let (client_end, server_end) = ipc_test::pair();
        let mut sink = RouterSink::default();
        std::thread::scope(|scope| {
            let server = scope.spawn(|| {
                server_end
                    .serve(|frame| file_system_mount::dispatch(&mut sink, frame))
                    .expect("dispatch ok");
            });

            let client = file_system_mount::Client::new(client_end);
            assert_eq!(client.mount("/boot", SESSION), Ok(MountEnded::Replaced));
            server.join().expect("server joins");
        });
        assert_eq!(
            sink.mounted,
            [("/boot".into(), SESSION.into_handle().raw())]
        );
    }
}
