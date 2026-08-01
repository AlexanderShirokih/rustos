//! Admin-контракты процессов файловой подсистемы: порты, отданные тому, кто эти процессы поднял,
//! и клиентам недоступные. По ним приходят стартовые capability, а у файловой системы - ещё и
//! запросы на установление сессии.

use crate::{error::FileSystemError, limits::PATH_MAX};

/// Управление маршрутизатором имён со стороны того, кто его поднял. Таблица монтирования - тоже
/// здесь: клиентской капе сервисного порта она недоступна по построению.
#[ipc::protocol(name = "NamespaceAdmin")]
pub trait NamespaceAdmin {
    /// Отдаёт маршрутизатору канал логирования, регион userland-образа и его сервисный порт.
    ///
    /// Сервисный порт создаёт поднявший: он оставляет себе минт-капу без `READ` и переносит
    /// корневую capability целиком: получателем на порту числится один маршрутизатор.
    #[cast]
    fn start(&self, log: ipc::wire::Cap, image: ipc::wire::Cap, service: ipc::wire::Cap);

    /// Поднимает файловую систему из записи образа `program` и монтирует её под префикс `path`.
    /// Занятый префикс - `AlreadyExists`.
    #[call]
    fn mount(
        &self,
        path: ipc::wire::Str<{ PATH_MAX }>,
        program: ipc::wire::Str<{ PATH_MAX }>,
    ) -> Result<u32, FileSystemError>;

    /// Снимает монтирование с префикса `path`. Незанятый префикс - `NotFound`.
    #[call]
    fn unmount(&self, path: ipc::wire::Str<{ PATH_MAX }>) -> Result<u32, FileSystemError>;
}

/// Управление файловой системой со стороны того, кто её поднял.
#[ipc::protocol(name = "FileSystemAdmin")]
pub trait FileSystemAdmin {
    /// Отдаёт серверу канал логирования и регион userland-образа. Сервисный порт сервер создаёт
    /// сам: минтить сессии вправе только тот, кто отвечает за допуск к файловой системе.
    #[cast]
    fn start(&self, log: ipc::wire::Cap, image: ipc::wire::Cap);

    /// Заводит сессию и отдаёт capability сервисного порта под её значком. Значок назначает
    /// сервер: политика допуска принадлежит владельцу файловой системы, и место отказа - само
    /// установление сессии.
    ///
    /// Значения значков не переиспользуются: это отделяет отставший кадр ушедшей сессии от
    /// живой.
    #[call]
    fn bind(&self) -> Result<ipc::wire::Cap, FileSystemError>;
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU32;
    use std::{string::ToString, vec::Vec};

    use ipc::wire::{Cap, Str};
    use ipc_test::MockEnd;

    use super::{
        FileSystemAdminClient, FileSystemAdminService, FileSystemError, NamespaceAdminClient,
        NamespaceAdminService, PATH_MAX, dispatch_file_system_admin, dispatch_namespace_admin,
    };

    const LOG_CAP: Cap = cap(0x21);
    const IMAGE_CAP: Cap = cap(0x22);
    const SERVICE_CAP: Cap = cap(0x23);

    const SESSION_CAP: Cap = cap(0x24);

    #[derive(Default)]
    struct Sink {
        received: Option<[Cap; 2]>,
        binds: u32,
    }

    #[derive(Default)]
    struct RouterSink {
        received: Option<[Cap; 3]>,
        mounted: Vec<std::string::String>,
    }

    impl FileSystemAdminService for Sink {
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

    impl NamespaceAdminService for RouterSink {
        fn start(&mut self, log: Cap, image: Cap, service: Cap) {
            self.received = Some([log, image, service]);
        }

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
        let client = FileSystemAdminClient::new(client_end);
        client.start(LOG_CAP, IMAGE_CAP).expect("start write");
        let mut sink = Sink::default();
        dispatch_file_system_admin(&mut sink, &server_end).expect("dispatch ok");
        assert_eq!(sink.received, Some([LOG_CAP, IMAGE_CAP]));
    }

    #[test]
    fn namespace_start_carries_three_caps_in_order() {
        let (client_end, server_end) = MockEnd::pair();
        let client = NamespaceAdminClient::new(client_end);
        client
            .start(LOG_CAP, IMAGE_CAP, SERVICE_CAP)
            .expect("start write");
        let mut sink = RouterSink::default();
        dispatch_namespace_admin(&mut sink, &server_end).expect("dispatch ok");
        assert_eq!(sink.received, Some([LOG_CAP, IMAGE_CAP, SERVICE_CAP]));
    }

    #[test]
    fn mount_and_unmount_ride_the_admin_port() {
        let (client_end, server_end) = MockEnd::pair();
        let mut sink = RouterSink::default();
        std::thread::scope(|scope| {
            let server = scope.spawn(|| {
                for _ in 0..2 {
                    ipc::Transport::wait_readable(&server_end, u64::MAX).expect("server wait");
                    dispatch_namespace_admin(&mut sink, &server_end).expect("dispatch ok");
                }
            });

            let client = NamespaceAdminClient::new(client_end);
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
                    ipc::Transport::wait_readable(&server_end, u64::MAX).expect("server wait");
                    dispatch_file_system_admin(&mut sink, &server_end).expect("dispatch ok");
                }
            });

            let client = FileSystemAdminClient::new(client_end);
            assert_eq!(client.bind().expect("call"), Ok(SESSION_CAP));
            assert_eq!(client.bind().expect("call"), Err(FileSystemError::NoSpace));
            server.join().expect("server joins");
        });
    }
}
