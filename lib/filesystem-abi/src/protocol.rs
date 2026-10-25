//! Управляющий порт файлового сервиса открывает дескрипторы, передаёт метаданные
//! и capability. Чтение, запись и перечисление выполняются через общий обмен IoRecord.
//!
//! Порт равен монтированию. Путь отсчитывается от корня монтирования, а сервер не отслеживает
//! собственный префикс и соседей. Префикс снимает клиент по длине, которую отдал маршрутизатор
//! имён.
//!
//! Сессия выполняет одну операцию переноса до её завершения либо отмены. Операции разных
//! сессий над одним узлом согласует сервер. Управляющие вызовы, не зависящие от активного
//! переноса, могут завершаться независимо. Клиент задаёт нужный ему порядок вызовов.
//! Значение закрытого дескриптора не переназначается другому узлу. Отставший вызов по нему
//! получает `BadDescriptor`.

use ipc::wire::{BODY_MAX, FIELD_OVERHEAD};

use crate::{
    descriptor::NodeDescriptor,
    error::FileSystemError,
    flags::OpenFlags,
    kind::NodeKind,
    limits::PATH_MAX,
    mode::AccessMode,
    reply::{NodeInfo, Opened},
};

const _: () = assert!(
    // Слагаемое `1` - терминатор тела кадра.
    1 + FIELD_OVERHEAD
        + PATH_MAX
        + FIELD_OVERHEAD
        + size_of::<u8>()
        + FIELD_OVERHEAD
        + size_of::<u32>()
        <= BODY_MAX,
    "open frame exceeds the body budget"
);

/// Смещение, с которым операция Write выбирает актуальный конец узла на сервере.
pub const APPEND_OFFSET: u64 = u64::MAX;

ipc::protocol! {
    /// Доступ к дереву имён и содержимому узлов. Операции без собственного результата
    /// возвращают `0`.
    pub mod file_system {
        name = "FileSystem";

        /// Устанавливает единственную пару колец сессии. Повторная установка даёт AlreadyExists.
        /// Корни колец и сигналов удерживает сервер; capability несут только управляющие записи.
        call(defer) fn start_io() -> own Result<crate::IoBootstrap, FileSystemError>;

        /// Принимает capability поколения, объявленного записью Register. Другой номер либо
        /// отсутствие регистрации дают NoBuffer. Клиент подтверждает отображение через Registered.
        call fn take_io_region(region: u64) -> own Result<ipc::wire::Cap<syscall::kind::MemoryRegion>, FileSystemError>;

        /// Выдаёт scratch установленного обмена размера `IO_SCRATCH_BYTES` с правом записи.
        /// Сессия без обмена получает NoBuffer.
        call fn take_io_scratch() -> own Result<ipc::wire::Cap<syscall::kind::MemoryRegion>, FileSystemError>;

        /// Открывает узел по пути внутри монтирования. `mode` задаёт режим доступа дескриптора,
        /// `flags` - действия над деревом имён. Дескриптор держит узел до `close` и адресует тот
        /// же узел после снятия имени и после переноса узла под другое имя.
        ///
        /// Отсутствие узла без `CREATE` - `NotFound`, `CREATE` вместе с `EXCLUSIVE` поверх
        /// существующего узла - `AlreadyExists`, `DIRECTORY` на файле - `NotADirectory`,
        /// `TRUNCATE` без права записи - `WrongMode`. Режим записи в неизменяемом дереве -
        /// `ReadOnly`, исчерпанный потолок дескрипторов сессии - `TooManyOpenFiles`.
        call async fn open(
            path: &str,
            mode: AccessMode,
            flags: OpenFlags,
        ) -> Result<Opened, FileSystemError>;

        /// Закрывает дескриптор сессии, снимая с узла держателя. Файловая система освобождает
        /// узел без имени и без держателей вместе с его байтами. Незнакомый либо уже закрытый
        /// дескриптор - `BadDescriptor`.
        call fn close(fd: NodeDescriptor) -> Result<u32, FileSystemError>;

        /// Задаёт длину узла. Байты за `size` отбрасываются, расширение дописывает нули.
        /// Дескриптор без права записи - `WrongMode`, дескриптор каталога - `IsADirectory`,
        /// длина сверх предела файловой системы - `NoSpace`.
        call async fn set_size(fd: NodeDescriptor, size: u64) -> Result<u32, FileSystemError>;

        /// Отдаёт метаданные узла, закреплённого за дескриптором. Дескриптор без права чтения -
        /// `WrongMode`.
        call fn stat(fd: NodeDescriptor) -> Result<NodeInfo, FileSystemError>;

        /// Переносит узел из `from` под имя `to` внутри монтирования. Занятое имя `to` перенос
        /// замещает, и замещённый узел остаётся без имени. Совпадение `from` с `to` дерево не
        /// меняет.
        ///
        /// Отсутствие `from` - `NotFound`, каталог под именем `to` - `IsADirectory`, перенос
        /// каталога внутрь собственного поддерева - `InvalidPath`, путь корня сессии -
        /// `InvalidPath`, неизменяемое дерево - `ReadOnly`.
        call fn rename(
            from: &str,
            to: &str,
        ) -> Result<u32, FileSystemError>;

        /// Снимает с узла имя, сверяя вид узла с `expected`. Ожидание файла при фактическом
        /// каталоге - `IsADirectory`, ожидание каталога при фактическом файле - `NotADirectory`,
        /// каталог с записями - `NotEmpty`, путь корня сессии - `InvalidPath`, неизменяемое
        /// дерево - `ReadOnly`.
        ///
        /// Открытые дескрипторы снятого узла остаются рабочими, и узел освобождается с закрытием
        /// последнего из них.
        call fn unlink(
            path: &str,
            expected: NodeKind,
        ) -> Result<u32, FileSystemError>;

        /// Создаёт файл под `path` поверх переданного RW-региона без копирования. Владение
        /// регионом переходит серверу, `size` задаёт начальную длину файла в границах ёмкости
        /// региона. Ёмкость закреплена за файлом, и запись за её предел отвечает `NoSpace`.
        ///
        /// Регион с длиной не кратной странице либо без прав RW - `NotSupported`, `size` сверх
        /// ёмкости региона - `NoSpace`, занятое имя - `AlreadyExists`, неизменяемое дерево -
        /// `ReadOnly`.
        call fn create_backed(
            path: &str,
            own region: ipc::wire::Cap<syscall::kind::MemoryRegion>,
            size: u64,
        ) -> Result<u32, FileSystemError>;

        /// Отдаёт capability на регион с содержимым узла. Права выводятся из режима
        /// дескриптора. Пустой узел и бекенд без такого региона - `NotSupported`, дескриптор без
        /// права чтения - `WrongMode`.
        ///
        /// Доставленный ответ закрепляет ёмкость узла за регионом. Запись и `set_size` за
        /// пределом этой ёмкости отвечают `NoSpace`.
        call(defer) async fn backing_memory(
            fd: NodeDescriptor,
        ) -> own Result<ipc::wire::Cap<syscall::kind::MemoryRegion>, FileSystemError>;

        /// Отдаёт capability свежей сессии, корнем которой становится узел `path`. Путь
        /// отсчитывается от корня вызванной сессии, и окно производной сессии только сужается.
        /// Отсутствие узла - `NotFound`, путь за `PATH_MAX` вместе с окном - `InvalidPath`.
        ///
        /// Дескрипторы и обмен данными у производной сессии свои.
        call fn open_root(
            path: &str,
        ) -> own Result<ipc::wire::Session<crate::file_system::Protocol>, FileSystemError>;
    }
}

#[cfg(test)]
mod tests {
    use std::{
        string::{String, ToString},
        sync::{Arc, Mutex},
        thread,
        vec::Vec,
    };

    use ipc::{CallError, wire::Cap};
    use ipc_test::{MockHandle, MockReply, MockResources, complete};

    use super::{
        AccessMode, FileSystemError, NodeDescriptor, NodeInfo, NodeKind, OpenFlags, Opened,
        PATH_MAX, file_system,
    };

    const BUFFER_CAP: MockHandle<syscall::kind::MemoryRegion> = MockHandle::new(0x11);
    const BACKING_CAP: MockHandle<syscall::kind::MemoryRegion> = MockHandle::new(0x22);
    const SCOPED_SESSION: ipc::wire::Session<
        file_system::Protocol,
        MockHandle<syscall::kind::Port>,
    > = ipc::wire::Session::from_handle(MockHandle::new(0x33));
    const IMPORT_CAP: MockHandle<syscall::kind::MemoryRegion> = MockHandle::new(0x44);
    const DONE: u32 = 0;
    const FD: NodeDescriptor = NodeDescriptor::from_raw(7);
    const OPENED: Opened = Opened {
        fd: FD,
        info: NodeInfo {
            kind: NodeKind::File,
            size: 4096,
            volume: 0x5EED,
            node: 9,
        },
    };
    const NODE_INFO: NodeInfo = NodeInfo {
        kind: NodeKind::Directory,
        size: 128,
        volume: 0x5EED,
        node: 3,
    };

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Call {
        StartIo,
        TakeScratch,
        ClaimRegion {
            generation: u64,
        },
        Open {
            path: String,
            mode: AccessMode,
            flags: OpenFlags,
        },
        Close {
            fd: NodeDescriptor,
        },
        SetSize {
            fd: NodeDescriptor,
            size: u64,
        },
        Stat {
            fd: NodeDescriptor,
        },
        Rename {
            from: String,
            to: String,
        },
        Unlink {
            path: String,
            expected: NodeKind,
        },
        CreateBacked {
            path: String,
            region: u32,
            size: u64,
        },
        BackingMemory {
            fd: NodeDescriptor,
        },
        OpenRoot {
            path: String,
        },
    }

    struct Recorder {
        calls: Arc<Mutex<Vec<Call>>>,
    }

    impl Recorder {
        fn record(&self, call: Call) {
            self.calls.lock().expect("mutex not poisoned").push(call);
        }
    }

    #[allow(clippy::unused_async_trait_impl)]
    impl file_system::Service<MockResources> for Recorder {
        fn start_io(&self, _frame: &MockResources, reply: file_system::start_io::Reply<MockReply>) {
            self.record(Call::StartIo);
            let _ = reply.send(Ok::<_, FileSystemError>(crate::IoBootstrap {
                requests: BUFFER_CAP,
                request_ready: MockHandle::<syscall::kind::Signal>::new(0x51),
                responses: MockHandle::<syscall::kind::MemoryRegion>::new(0x12),
                response_ready: MockHandle::<syscall::kind::Signal>::new(0x52),
            }));
        }

        fn take_io_region(
            &self,
            _frame: &MockResources,
            region: u64,
        ) -> impl ipc::IntoWireValue<
            MockResources,
            Read = Result<Cap<syscall::kind::MemoryRegion>, FileSystemError>,
        > {
            self.record(Call::ClaimRegion { generation: region });
            if region == 0 {
                return Err(FileSystemError::NoBuffer);
            }

            Ok(BUFFER_CAP)
        }

        fn take_io_scratch(
            &self,
            _frame: &MockResources,
        ) -> impl ipc::IntoWireValue<
            MockResources,
            Read = Result<Cap<syscall::kind::MemoryRegion>, FileSystemError>,
        > {
            self.record(Call::TakeScratch);
            Ok(BUFFER_CAP)
        }

        async fn open(
            &self,
            _frame: &MockResources,
            path: &str,
            mode: AccessMode,
            flags: OpenFlags,
        ) -> Result<Opened, FileSystemError> {
            self.record(Call::Open {
                path: path.to_string(),
                mode,
                flags,
            });
            Ok(OPENED)
        }

        fn close(
            &self,
            _frame: &MockResources,
            fd: NodeDescriptor,
        ) -> Result<u32, FileSystemError> {
            self.record(Call::Close { fd });
            Ok(DONE)
        }

        async fn set_size(
            &self,
            _frame: &MockResources,
            fd: NodeDescriptor,
            size: u64,
        ) -> Result<u32, FileSystemError> {
            self.record(Call::SetSize { fd, size });
            Ok(DONE)
        }

        fn stat(
            &self,
            _frame: &MockResources,
            fd: NodeDescriptor,
        ) -> Result<NodeInfo, FileSystemError> {
            self.record(Call::Stat { fd });
            Ok(NODE_INFO)
        }

        fn rename(
            &self,
            _frame: &MockResources,
            from: &str,
            to: &str,
        ) -> Result<u32, FileSystemError> {
            self.record(Call::Rename {
                from: from.to_string(),
                to: to.to_string(),
            });
            Ok(DONE)
        }

        fn unlink(
            &self,
            _frame: &MockResources,
            path: &str,
            expected: NodeKind,
        ) -> Result<u32, FileSystemError> {
            self.record(Call::Unlink {
                path: path.to_string(),
                expected,
            });
            Ok(DONE)
        }

        fn create_backed(
            &self,
            _frame: &MockResources,
            path: &str,
            region: MockHandle<syscall::kind::MemoryRegion>,
            size: u64,
        ) -> Result<u32, FileSystemError> {
            self.record(Call::CreateBacked {
                path: path.to_string(),
                region: region.raw(),
                size,
            });
            Ok(DONE)
        }

        async fn backing_memory(
            &self,
            _frame: &MockResources,
            fd: NodeDescriptor,
            reply: file_system::backing_memory::Reply<MockReply>,
        ) {
            self.record(Call::BackingMemory { fd });
            let _ = reply.send(Ok::<_, FileSystemError>(BACKING_CAP));
        }

        fn open_root(
            &self,
            _frame: &MockResources,
            path: &str,
        ) -> impl ipc::IntoWireValue<
            MockResources,
            Read = Result<ipc::wire::Session<file_system::Protocol>, FileSystemError>,
        > {
            self.record(Call::OpenRoot {
                path: path.to_string(),
            });
            Ok(SCOPED_SESSION)
        }
    }

    fn with_service<R>(
        body: impl FnOnce(&file_system::Client<ipc_test::MockChannel>) -> R,
    ) -> (R, Vec<Call>) {
        let (client_end, server_end) = ipc_test::pair();
        let client = file_system::Client::new(client_end);
        let calls = Arc::new(Mutex::new(Vec::new()));
        let result = thread::scope(|scope| {
            let service = Recorder {
                calls: Arc::clone(&calls),
            };
            scope.spawn(move || {
                let _ = server_end.serve_all(|frame| {
                    complete(file_system::dispatch(
                        &service,
                        ipc::OwnedIncoming::new(frame).expect("frame fits"),
                    ))
                });
            });
            let result = body(&client);
            drop(client);
            result
        });
        let calls = Arc::into_inner(calls)
            .expect("service thread released the journal")
            .into_inner()
            .expect("mutex not poisoned");
        (result, calls)
    }

    fn path(text: &str) -> &str {
        text
    }

    #[test]
    fn every_operation_carries_its_parameters_and_answer() {
        let flags = OpenFlags::CREATE | OpenFlags::EXCLUSIVE;
        let (answers, calls) = with_service(|client| {
            [
                client.start_io().wait().map(|ends| {
                    assert_eq!(ends.requests.raw(), 0x11);
                    assert_eq!(ends.responses.raw(), 0x12);
                    assert_eq!(ends.request_ready.raw(), 0x51);
                    assert_eq!(ends.response_ready.raw(), 0x52);
                    0
                }),
                client.take_io_region(7).wait().map(|_| 0),
                client.take_io_region(0).wait().map(|_| 0),
                client
                    .open(path("/data/sample"), AccessMode::ReadWrite, flags)
                    .wait()
                    .map(|opened| opened.info.size as u32),
                client.close(FD).wait(),
                client.set_size(FD, 8192).wait(),
                client.stat(FD).wait().map(|info| info.size as u32),
                client.rename(path("/store/a"), path("/store/b")).wait(),
                client.unlink(path("/store/a"), NodeKind::File).wait(),
                client.backing_memory(FD).wait().map(|_| 0),
                client
                    .create_backed(path("/store/shared"), IMPORT_CAP, 123)
                    .wait(),
                client.open_root(path("/window")).wait().map(|_| 0),
            ]
        });
        assert_eq!(
            answers,
            [
                Ok(0),
                Ok(0),
                Err(CallError::Remote(FileSystemError::NoBuffer)),
                Ok(OPENED.info.size as u32),
                Ok(DONE),
                Ok(DONE),
                Ok(NODE_INFO.size as u32),
                Ok(DONE),
                Ok(DONE),
                Ok(0),
                Ok(DONE),
                Ok(0)
            ]
        );
        assert_eq!(
            calls,
            [
                Call::StartIo,
                Call::ClaimRegion { generation: 7 },
                Call::ClaimRegion { generation: 0 },
                Call::Open {
                    path: "/data/sample".to_string(),
                    mode: AccessMode::ReadWrite,
                    flags
                },
                Call::Close { fd: FD },
                Call::SetSize { fd: FD, size: 8192 },
                Call::Stat { fd: FD },
                Call::Rename {
                    from: "/store/a".to_string(),
                    to: "/store/b".to_string()
                },
                Call::Unlink {
                    path: "/store/a".to_string(),
                    expected: NodeKind::File
                },
                Call::BackingMemory { fd: FD },
                Call::CreateBacked {
                    path: "/store/shared".to_string(),
                    region: IMPORT_CAP.raw(),
                    size: 123
                },
                Call::OpenRoot {
                    path: "/window".to_string()
                },
            ]
        );
    }

    #[test]
    fn path_at_bound_fits_the_frame() {
        let path = "/".to_string() + &"p".repeat(PATH_MAX - 1);
        let (result, calls) =
            with_service(|client| client.open(&path, AccessMode::Read, OpenFlags::NONE).wait());
        assert_eq!(result, Ok(OPENED));
        assert_eq!(
            calls,
            [Call::Open {
                path,
                mode: AccessMode::Read,
                flags: OpenFlags::NONE,
            }]
        );
    }
}
