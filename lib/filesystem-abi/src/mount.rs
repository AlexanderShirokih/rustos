//! Контракт монтирования файловых систем по абсолютному пути.

use crate::error::FileSystemError;

ipc::wire_enum! {
    /// Причина снятия монтирования.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum MountEnded {
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
        /// после отказа `PeerClosed` на запрос к порту файловой системы либо монтирования
        /// другой сессии под тем же префиксом. Закрытие сессии отменяет вызов с `PeerClosed`.
        call async fn mount(
            path: &str,
            own filesystem: ipc::wire::Session<crate::file_system::Protocol>,
        ) -> Result<MountEnded, FileSystemError>;
    }
}

#[cfg(test)]
mod tests {
    use core::cell::RefCell;
    use std::vec::Vec;

    use ipc::wire::Session;
    use ipc_test::{MockHandle, MockResources, complete};
    use syscall::kind;

    use super::{FileSystemError, MountEnded, file_system_mount};
    use crate::file_system;

    type FileSystemSession = Session<file_system::Protocol, MockHandle<kind::Port>>;

    const SESSION: FileSystemSession = Session::from_handle(MockHandle::new(0x41));

    #[derive(Default)]
    struct RouterSink {
        mounted: RefCell<Vec<(std::string::String, u32)>>,
    }

    #[allow(clippy::unused_async_trait_impl)]
    impl file_system_mount::Service<MockResources> for RouterSink {
        async fn mount(
            &self,
            _frame: &MockResources,
            path: &str,
            filesystem: FileSystemSession,
        ) -> Result<MountEnded, FileSystemError> {
            self.mounted
                .borrow_mut()
                .push((path.into(), filesystem.into_handle().raw()));
            Ok(MountEnded::Replaced)
        }
    }

    #[test]
    fn a_mount_carries_its_path_and_file_system_capability() {
        let (client_end, server_end) = ipc_test::pair();
        let sink = RouterSink::default();
        let sink = std::thread::scope(|scope| {
            let server = scope.spawn(move || {
                server_end
                    .serve(|frame| {
                        complete(file_system_mount::dispatch(
                            &sink,
                            ipc::OwnedIncoming::new(frame).expect("frame fits"),
                        ))
                    })
                    .expect("dispatch ok");
                sink
            });

            let client = file_system_mount::Client::new(client_end);
            assert_eq!(
                client.mount("/boot", SESSION).wait(),
                Ok(MountEnded::Replaced)
            );
            server.join().expect("server joins")
        });
        assert_eq!(
            sink.mounted.into_inner(),
            [("/boot".into(), SESSION.into_handle().raw())]
        );
    }
}
