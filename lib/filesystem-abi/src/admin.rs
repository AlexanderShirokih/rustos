//! Startup- и admin-контракты процессов файловой подсистемы. Startup одноразово
//! передаёт исходные capability, admin-порты живут всё время работы процесса и клиентам
//! недоступны.

use crate::{error::FileSystemError, limits::PATH_MAX};

/// Одноразовый startup-кадр маршрутизатора имён.
#[ipc::protocol(name = "NamespaceStartup")]
pub trait NamespaceStartup {
    /// Отдаёт маршрутизатору канал логирования, регион userland-образа, сервисный
    /// и admin-порты. Корневые capability обоих портов переносятся целиком.
    #[cast]
    fn start(
        &self,
        log: ipc::wire::Cap,
        image: ipc::wire::Cap,
        service: ipc::wire::Cap,
        admin: ipc::wire::Cap,
    );
}

/// Capability, принятые из одноразового startup-кадра маршрутизатора.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NamespaceStartupFrame {
    /// Делегированная log-сессия.
    pub log: ipc::wire::Cap,
    /// Read-only userland image.
    pub image: ipc::wire::Cap,
    /// Серверная сторона namespace service Port.
    pub service: ipc::wire::Cap,
    /// Серверная сторона постоянного namespace admin Port.
    pub admin: ipc::wire::Cap,
}

impl NamespaceStartupService for Option<NamespaceStartupFrame> {
    fn start(
        &mut self,
        log: ipc::wire::Cap,
        image: ipc::wire::Cap,
        service: ipc::wire::Cap,
        admin: ipc::wire::Cap,
    ) {
        *self = Some(NamespaceStartupFrame {
            log,
            image,
            service,
            admin,
        });
    }
}

/// Управление маршрутизатором имён со стороны того, кто его поднял. Таблица монтирования - тоже
/// здесь: клиентской капе сервисного порта она недоступна по построению.
#[ipc::protocol(name = "NamespaceAdmin")]
pub trait NamespaceAdmin {
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
    use std::{cell::Cell, rc::Rc, string::ToString, vec::Vec};

    use ipc::{
        IntoWireHandle,
        wire::{Cap, IpcError, Str},
    };
    use ipc_test::MockEnd;

    use super::{
        FileSystemAdminClient, FileSystemAdminService, FileSystemError, NamespaceAdminClient,
        NamespaceAdminService, NamespaceStartupClient, NamespaceStartupFrame, PATH_MAX,
        dispatch_file_system_admin, dispatch_namespace_admin, dispatch_namespace_startup,
    };

    const LOG_CAP: Cap = cap(0x21);
    const IMAGE_CAP: Cap = cap(0x22);
    const SERVICE_CAP: Cap = cap(0x23);

    const ADMIN_CAP: Cap = cap(0x24);
    const SESSION_CAP: Cap = cap(0x25);

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Fate {
        Pending,
        Relinquished,
        Dropped,
    }

    struct FakeHandle {
        id: u32,
        fate: Rc<Cell<Fate>>,
    }

    impl IntoWireHandle for FakeHandle {
        fn wire_id(&self) -> u32 {
            self.id
        }

        fn relinquish(self) {
            self.fate.set(Fate::Relinquished);
            core::mem::forget(self);
        }
    }

    impl Drop for FakeHandle {
        fn drop(&mut self) {
            self.fate.set(Fate::Dropped);
        }
    }

    #[derive(Default)]
    struct Sink {
        received: Option<[Cap; 2]>,
        binds: u32,
    }

    #[derive(Default)]
    struct RouterSink {
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

    fn fake(id: u32) -> (FakeHandle, Rc<Cell<Fate>>) {
        let fate = Rc::new(Cell::new(Fate::Pending));
        (
            FakeHandle {
                id,
                fate: Rc::clone(&fate),
            },
            fate,
        )
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
    fn namespace_startup_is_separate_and_carries_four_caps_in_order() {
        let (client_end, server_end) = MockEnd::pair();
        let client = NamespaceStartupClient::new(client_end);
        client
            .start(LOG_CAP, IMAGE_CAP, SERVICE_CAP, ADMIN_CAP)
            .expect("start write");
        let mut sink = None;
        dispatch_namespace_startup(&mut sink, &server_end).expect("dispatch ok");
        assert_eq!(
            sink,
            Some(NamespaceStartupFrame {
                log: LOG_CAP,
                image: IMAGE_CAP,
                service: SERVICE_CAP,
                admin: ADMIN_CAP,
            })
        );
    }

    #[test]
    fn namespace_startup_closes_all_caps_when_the_cast_is_not_delivered() {
        let (client_end, server_end) = MockEnd::pair();
        drop(server_end);
        let client = NamespaceStartupClient::new(client_end);
        let (log, log_fate) = fake(0x31);
        let (image, image_fate) = fake(0x32);
        let (service, service_fate) = fake(0x33);
        let (admin, admin_fate) = fake(0x34);

        assert_eq!(
            client.start(log, image, service, admin),
            Err(IpcError::PeerClosed)
        );
        assert_eq!(
            [
                log_fate.get(),
                image_fate.get(),
                service_fate.get(),
                admin_fate.get(),
            ],
            [Fate::Dropped; 4]
        );
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
