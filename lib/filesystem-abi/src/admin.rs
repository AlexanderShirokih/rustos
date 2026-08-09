//! Admin-контракты процессов файловой подсистемы: они живут всё время работы процесса
//! и клиентским капам недоступны.

use crate::{error::FileSystemError, limits::PATH_MAX};

ipc::protocol! {
    /// Управление маршрутизатором имён со стороны того, кто его поднял. Таблица монтирования - тоже
    /// здесь: клиентской капе сервисного порта она недоступна по построению.
    pub mod namespace_admin {
        name = "NamespaceAdmin";

        /// Поднимает файловую систему из записи образа `program` и монтирует её под префикс
        /// `path`. Занятый префикс - `AlreadyExists`.
        call fn mount(
            path: ipc::wire::Str<{ PATH_MAX }>,
            program: ipc::wire::Str<{ PATH_MAX }>,
        ) -> Result<u32, FileSystemError>;

        /// Снимает монтирование с префикса `path`. Незанятый префикс - `NotFound`.
        call fn unmount(path: ipc::wire::Str<{ PATH_MAX }>) -> Result<u32, FileSystemError>;
    }
}

ipc::protocol! {
    /// Управление самой файловой системой: запуск и установление сессий.
    pub mod file_system_admin {
        name = "FileSystemAdmin";

        /// Отдаёт серверу канал логирования и регион userland-образа. Сервисный порт сервер
        /// создаёт сам: минтить сессии вправе только тот, кто отвечает за допуск к файловой
        /// системе.
        cast fn start(log: Cap, image: Cap);

        /// Заводит сессию и отдаёт capability сервисного порта под её значком. Значок
        /// назначает сервер: политика допуска принадлежит владельцу файловой системы, и место
        /// отказа - само установление сессии.
        ///
        /// Значения значков не переиспользуются: это отделяет отставший кадр ушедшей сессии
        /// от живой.
        call fn bind() -> Result<ipc::wire::Cap, FileSystemError>;
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU32;
    use std::{string::ToString, vec::Vec};

    use ipc::wire::{Cap, Str};
    use ipc_test::MockEnd;

    use super::{FileSystemError, PATH_MAX, file_system_admin, namespace_admin};

    const LOG_CAP: Cap = cap(0x21);
    const IMAGE_CAP: Cap = cap(0x22);
    const SESSION_CAP: Cap = cap(0x25);

    #[derive(Default)]
    struct Sink {
        received: Option<[Cap; 2]>,
        binds: u32,
    }

    #[derive(Default)]
    struct RouterSink {
        mounted: Vec<std::string::String>,
    }

    impl file_system_admin::Service for Sink {
        fn start(&mut self, log: Cap, image: Cap) {
            self.received = Some([log, image]);
        }

        fn bind(&mut self) -> Result<Cap, FileSystemError> {
            self.binds += 1;
            // Вторая сессия сверх квоты этой заглушки не заводится.
            if self.binds > 1 {
                return Err(FileSystemError::NoSpace);
            }
            Ok(SESSION_CAP)
        }
    }

    impl namespace_admin::Service for RouterSink {
        fn mount(
            &mut self,
            path: Str<'_, PATH_MAX>,
            program: Str<'_, PATH_MAX>,
        ) -> Result<u32, FileSystemError> {
            self.mounted
                .push(std::format!("mount {} {}", path.as_str(), program.as_str()));
            Ok(0)
        }

        fn unmount(&mut self, path: Str<'_, PATH_MAX>) -> Result<u32, FileSystemError> {
            self.mounted.push(std::format!("unmount {}", path.as_str()));
            Ok(0)
        }
    }

    const fn cap(raw: u32) -> Cap {
        Cap::from_raw(NonZeroU32::new(raw).expect("non-zero"))
    }

    #[test]
    fn file_system_start_carries_two_caps_in_order() {
        let (client_end, server_end) = MockEnd::pair();
        let client = file_system_admin::Client::new(client_end);
        client.start(LOG_CAP, IMAGE_CAP).expect("start write");
        let mut sink = Sink::default();
        file_system_admin::dispatch(&mut sink, &server_end).expect("dispatch ok");
        assert_eq!(sink.received, Some([LOG_CAP, IMAGE_CAP]));
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
            let program = Str::new("/boot/ramfs").expect("path within the bound");
            assert_eq!(client.mount(path, program).expect("call"), Ok(0));
            assert_eq!(client.unmount(path).expect("call"), Ok(0));
            server.join().expect("server joins");
        });
        assert_eq!(
            sink.mounted,
            [
                "mount /tmp /boot/ramfs".to_string(),
                "unmount /tmp".to_string()
            ]
        );
    }

    #[test]
    fn bind_vends_the_session_capability_and_may_refuse() {
        let (client_end, server_end) = MockEnd::pair();
        let mut sink = Sink::default();
        std::thread::scope(|scope| {
            let server = scope.spawn(|| {
                for _ in 0..2 {
                    ipc::Transport::wait_readable(&server_end, ipc::Timeout::INFINITE)
                        .expect("server wait");
                    file_system_admin::dispatch(&mut sink, &server_end).expect("dispatch ok");
                }
            });

            let client = file_system_admin::Client::new(client_end);
            assert_eq!(client.bind().expect("call"), Ok(SESSION_CAP));
            assert_eq!(client.bind().expect("call"), Err(FileSystemError::NoSpace));
            server.join().expect("server joins");
        });
    }
}
