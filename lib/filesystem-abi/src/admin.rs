//! Admin-контракт маршрутизатора имён. Порт этого контракта отделён от сервисного, и клиентская
//! капа сервисного порта до него не достаёт.

use crate::{error::FileSystemError, limits::PATH_MAX};

ipc::protocol! {
    /// Управление маршрутизатором имён со стороны того, кто его поднял. Таблица монтирований
    /// меняется только отсюда.
    pub mod namespace_admin {
        name = "NamespaceAdmin";

        /// Монтирует под префикс `path` переданную сессию файловой системы. Занятый префикс -
        /// `AlreadyExists`, путь вне формы контракта - `InvalidPath`. Отказ монтирования капу
        /// сессии не возвращает, и отправитель её теряет.
        ///
        /// Принадлежность капы файловой системе проверяет клиент. Сессия чужого порта отвечает
        /// ему `Unresponsive` на разборе пути.
        call fn mount(
            path: ipc::wire::Str<{ PATH_MAX }>,
            session: Cap,
        ) -> Result<u32, FileSystemError>;

        /// Снимает монтирование с префикса `path` и закрывает капу его сессии. Незанятый
        /// префикс - `NotFound`. Сессии, выданные клиентам под этим префиксом, остаются
        /// рабочими.
        call fn unmount(path: ipc::wire::Str<{ PATH_MAX }>) -> Result<u32, FileSystemError>;
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU32;
    use std::{string::ToString, vec::Vec};

    use ipc::wire::{Cap, Str};
    use ipc_test::MockEnd;

    use super::{FileSystemError, PATH_MAX, namespace_admin};

    const SESSION: Cap = Cap::from_raw(NonZeroU32::new(0x41).expect("non-zero"));

    #[derive(Default)]
    struct RouterSink {
        mounted: Vec<std::string::String>,
    }

    impl namespace_admin::Service for RouterSink {
        fn mount(&mut self, path: Str<'_, PATH_MAX>, session: Cap) -> Result<u32, FileSystemError> {
            self.mounted
                .push(std::format!("mount {} {}", path.as_str(), session.raw()));
            Ok(0)
        }

        fn unmount(&mut self, path: Str<'_, PATH_MAX>) -> Result<u32, FileSystemError> {
            self.mounted.push(std::format!("unmount {}", path.as_str()));
            Ok(0)
        }
    }

    #[test]
    fn mount_and_unmount_ride_the_admin_port() {
        let (client_end, server_end) = MockEnd::pair();
        let mut sink = RouterSink::default();
        std::thread::scope(|scope| {
            let server = scope.spawn(|| {
                for _ in 0..2 {
                    ipc::Transport::wait_readable(&server_end, ipc::Timeout::INFINITE)
                        .expect("server wait");
                    namespace_admin::dispatch(&mut sink, &server_end).expect("dispatch ok");
                }
            });

            let client = namespace_admin::Client::new(client_end);
            let path = Str::new("/tmp").expect("path within the bound");
            assert_eq!(client.mount(path, SESSION).expect("call").value, Ok(0));
            assert_eq!(client.unmount(path).expect("call").value, Ok(0));
            server.join().expect("server joins");
        });
        assert_eq!(
            sink.mounted,
            [
                std::format!("mount /tmp {}", SESSION.raw()),
                "unmount /tmp".to_string()
            ]
        );
    }
}
