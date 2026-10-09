//! Контракт файлового сервиса: команды над деревом имён и узлами. Данные переменной длины идут
//! через буфер данных сессии, кадр содержит пути, длины и счётчики.
//!
//! Порт равен монтированию. Путь отсчитывается от корня монтирования, а сервер не отслеживает
//! собственный префикс и соседей. Префикс снимает клиент по длине, которую отдал маршрутизатор
//! имён.
//!
//! Порядок операций закреплён за вызовом, сессия его не добавляет. Несколько незавершённых вызовов
//! применяются в любом порядке, и исход их пересечения на одном узле контракт не определяет.
//! Клиент, которому нужен порядок, держит на сессии один незавершённый вызов. Ослабление идёт от
//! приёма несколькими потоками сервера. Значение закрытого дескриптора при этом не
//! переназначается другому узлу. Отставший вызов по нему получает `BadDescriptor`.

use ipc::wire::{BODY_MAX, FIELD_OVERHEAD};

use crate::{
    cursor::DirCursor,
    descriptor::NodeDescriptor,
    error::FileSystemError,
    flags::OpenFlags,
    kind::NodeKind,
    limits::PATH_MAX,
    mode::AccessMode,
    reply::{Listed, NodeInfo, Opened, Written},
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

/// Смещение дозаписи: `write` по нему кладёт байты за последним байтом узла.
pub const APPEND_OFFSET: u64 = u64::MAX;

ipc::protocol! {
    /// Файловый сервис: дерево имён и содержимое узлов. Операции без собственного результата
    /// возвращают `0`.
    pub mod file_system {
        name = "FileSystem";

        /// Выделяет сессии буфер данных под `size` байт и отдаёт RW-capability на него. Объём
        /// округляется вверх до страницы, нулевой либо превышающий потолок сервера - `NoSpace`.
        /// Сессия получает буфер один раз, повторный вызов - `AlreadyExists`. Регион остаётся за
        /// сервером, и закрытие сессии отзывает выданную capability.
        call fn acquire_buffer(size: u32) -> own Result<ipc::wire::Cap<syscall::kind::MemoryRegion>, FileSystemError>;

        /// Открывает узел по пути внутри монтирования. `mode` задаёт режим доступа дескриптора,
        /// `flags` - действия над деревом имён. Дескриптор держит узел до `close` и адресует тот
        /// же узел после снятия имени и после переноса узла под другое имя.
        ///
        /// Отсутствие узла без `CREATE` - `NotFound`, `CREATE` вместе с `EXCLUSIVE` поверх
        /// существующего узла - `AlreadyExists`, `DIRECTORY` на файле - `NotADirectory`,
        /// `TRUNCATE` без права записи - `WrongMode`. Режим записи в неизменяемом дереве -
        /// `ReadOnly`, исчерпанный потолок дескрипторов сессии - `TooManyOpenFiles`.
        call fn open(
            path: &str,
            mode: AccessMode,
            flags: OpenFlags,
        ) -> Result<Opened, FileSystemError>;

        /// Закрывает дескриптор сессии, снимая с узла держателя. Файловая система освобождает
        /// узел без имени и без держателей вместе с его байтами. Незнакомый либо уже закрытый
        /// дескриптор - `BadDescriptor`.
        call fn close(fd: NodeDescriptor) -> Result<u32, FileSystemError>;

        /// Кладёт в начало буфера сессии до `len` байт узла со смещения `offset`. Возвращает
        /// число прочитанных байт. Сервер заполняет нулями непрочитанный хвост запрошенных `len`
        /// байт, смещение от длины узла и дальше даёт ноль байт. Дескриптор без права чтения -
        /// `WrongMode`, сессия без буфера - `NoBuffer`, `len` сверх буфера - `BufferTooSmall`.
        call fn read(fd: NodeDescriptor, offset: u64, len: u32) -> Result<u32, FileSystemError>;

        /// Пишет в узел `len` байт из начала буфера сессии по смещению `offset`. Промежуток
        /// между прежней длиной узла и `offset` заполняется нулями. Смещение [`APPEND_OFFSET`]
        /// дозаписывает байты за последним байтом узла, и между сессиями такая дозапись
        /// неделима.
        ///
        /// Дескриптор без права записи - `WrongMode`, сессия без буфера - `NoBuffer`, `len`
        /// сверх буфера - `BufferTooSmall`, длина узла за пределом файловой системы - `NoSpace`.
        call fn write(
            fd: NodeDescriptor,
            offset: u64,
            len: u32,
        ) -> Result<Written, FileSystemError>;

        /// Задаёт длину узла. Байты за `size` отбрасываются, расширение дописывает нули.
        /// Дескриптор без права записи - `WrongMode`, дескриптор каталога - `IsADirectory`,
        /// длина сверх предела файловой системы - `NoSpace`.
        call fn set_size(fd: NodeDescriptor, size: u64) -> Result<u32, FileSystemError>;

        /// Отдаёт метаданные узла, закреплённого за дескриптором. Дескриптор без права чтения -
        /// `WrongMode`.
        call fn stat(fd: NodeDescriptor) -> Result<NodeInfo, FileSystemError>;

        /// Укладывает в буфер сессии записи каталога с позиции `from`, сколько их помещается.
        /// Перечисление продолжает вызов с `from` из [`Listed::next`]. Дескриптор файла -
        /// `NotADirectory`, дескриптор без права чтения - `WrongMode`, сессия без буфера -
        /// `NoBuffer`.
        call fn list(fd: NodeDescriptor, from: DirCursor) -> Result<Listed, FileSystemError>;

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
        call fn backing_memory(
            fd: NodeDescriptor,
        ) -> own Result<ipc::wire::Cap<syscall::kind::MemoryRegion>, FileSystemError>;

        /// Отдаёт capability свежей сессии, корнем которой становится узел `path`. Путь
        /// отсчитывается от корня вызванной сессии, и окно производной сессии только сужается.
        /// Отсутствие узла - `NotFound`, путь за `PATH_MAX` вместе с окном - `InvalidPath`.
        ///
        /// Дескрипторы и буфер данных у производной сессии свои.
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
    use ipc_test::{MockHandle, MockResources};

    use super::{
        AccessMode, DirCursor, FileSystemError, Listed, NodeDescriptor, NodeInfo, NodeKind,
        OpenFlags, Opened, PATH_MAX, Written, file_system,
    };

    const BUFFER_CAP: MockHandle<syscall::kind::MemoryRegion> = MockHandle::new(0x11);
    const BACKING_CAP: MockHandle<syscall::kind::MemoryRegion> = MockHandle::new(0x22);
    const SCOPED_SESSION: ipc::wire::Session<
        file_system::Protocol,
        MockHandle<syscall::kind::Port>,
    > = ipc::wire::Session::from_handle(MockHandle::new(0x33));
    const IMPORT_CAP: MockHandle<syscall::kind::MemoryRegion> = MockHandle::new(0x44);
    const READ_COUNT: u32 = 512;
    const DONE: u32 = 0;
    const FD: NodeDescriptor = NodeDescriptor::from_raw(7);
    const OPENED: Opened = Opened {
        fd: FD,
        kind: NodeKind::File,
        size: 4096,
    };
    const WRITTEN: Written = Written {
        count: 12,
        offset: 64,
    };
    const LISTED: Listed = Listed {
        count: 3,
        next: DirCursor::from_raw(5),
        end: false,
    };
    const NODE_INFO: NodeInfo = NodeInfo {
        kind: NodeKind::Directory,
        size: 128,
    };

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Call {
        AcquireBuffer {
            size: u32,
        },
        Open {
            path: String,
            mode: AccessMode,
            flags: OpenFlags,
        },
        Close {
            fd: NodeDescriptor,
        },
        Read {
            fd: NodeDescriptor,
            offset: u64,
            len: u32,
        },
        Write {
            fd: NodeDescriptor,
            offset: u64,
            len: u32,
        },
        SetSize {
            fd: NodeDescriptor,
            size: u64,
        },
        Stat {
            fd: NodeDescriptor,
        },
        List {
            fd: NodeDescriptor,
            from: DirCursor,
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

    impl file_system::Service<MockResources> for Recorder {
        fn acquire_buffer(
            &mut self,
            _frame: &MockResources,
            size: u32,
        ) -> impl ipc::IntoWireValue<
            MockResources,
            Read = Result<Cap<syscall::kind::MemoryRegion>, FileSystemError>,
        > {
            self.record(Call::AcquireBuffer { size });
            if size == 0 {
                return Err(FileSystemError::NoSpace);
            }
            Ok(BUFFER_CAP)
        }

        fn open(
            &mut self,
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
            &mut self,
            _frame: &MockResources,
            fd: NodeDescriptor,
        ) -> Result<u32, FileSystemError> {
            self.record(Call::Close { fd });
            Ok(DONE)
        }

        fn read(
            &mut self,
            _frame: &MockResources,
            fd: NodeDescriptor,
            offset: u64,
            len: u32,
        ) -> Result<u32, FileSystemError> {
            self.record(Call::Read { fd, offset, len });
            Ok(READ_COUNT)
        }

        fn write(
            &mut self,
            _frame: &MockResources,
            fd: NodeDescriptor,
            offset: u64,
            len: u32,
        ) -> Result<Written, FileSystemError> {
            self.record(Call::Write { fd, offset, len });
            Ok(WRITTEN)
        }

        fn set_size(
            &mut self,
            _frame: &MockResources,
            fd: NodeDescriptor,
            size: u64,
        ) -> Result<u32, FileSystemError> {
            self.record(Call::SetSize { fd, size });
            Ok(DONE)
        }

        fn stat(
            &mut self,
            _frame: &MockResources,
            fd: NodeDescriptor,
        ) -> Result<NodeInfo, FileSystemError> {
            self.record(Call::Stat { fd });
            Ok(NODE_INFO)
        }

        fn list(
            &mut self,
            _frame: &MockResources,
            fd: NodeDescriptor,
            from: DirCursor,
        ) -> Result<Listed, FileSystemError> {
            self.record(Call::List { fd, from });
            Ok(LISTED)
        }

        fn rename(
            &mut self,
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
            &mut self,
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
            &mut self,
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

        fn backing_memory(
            &mut self,
            _frame: &MockResources,
            fd: NodeDescriptor,
        ) -> impl ipc::IntoWireValue<
            MockResources,
            Read = Result<Cap<syscall::kind::MemoryRegion>, FileSystemError>,
        > {
            self.record(Call::BackingMemory { fd });
            Ok(BACKING_CAP)
        }

        fn open_root(
            &mut self,
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
            let mut service = Recorder {
                calls: Arc::clone(&calls),
            };
            scope.spawn(move || {
                let _ = server_end.serve_all(|frame| file_system::dispatch(&mut service, frame));
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

    /// Каждая операция доносит свои параметры до метода сервиса и возвращает его ответ.
    #[test]
    fn every_operation_carries_its_parameters_and_answer() {
        let flags = OpenFlags::CREATE | OpenFlags::EXCLUSIVE;
        let from = DirCursor::from_raw(2);
        let (answers, calls) = with_service(|client| {
            [
                client.acquire_buffer(4096).map(|_| 0),
                client.acquire_buffer(0).map(|_| 0),
                client
                    .open(path("/data/sample"), AccessMode::ReadWrite, flags)
                    .map(|opened| opened.size as u32),
                client.close(FD),
                client.read(FD, 1024, 4096),
                client.write(FD, u64::MAX, 12).map(|written| written.count),
                client.set_size(FD, 8192),
                client.stat(FD).map(|info| info.size as u32),
                client.list(FD, from).map(|listed| listed.count),
                client.rename(path("/store/a"), path("/store/b")),
                client.unlink(path("/store/a"), NodeKind::File),
                client.backing_memory(FD).map(|_| 0),
                client.create_backed(path("/store/shared"), IMPORT_CAP, 123),
                client.open_root(path("/window")).map(|_| 0),
            ]
        });

        assert_eq!(
            answers,
            [
                Ok(0),
                Err(CallError::Remote(FileSystemError::NoSpace)),
                Ok(OPENED.size as u32),
                Ok(DONE),
                Ok(READ_COUNT),
                Ok(WRITTEN.count),
                Ok(DONE),
                Ok(NODE_INFO.size as u32),
                Ok(LISTED.count),
                Ok(DONE),
                Ok(DONE),
                Ok(0),
                Ok(DONE),
                Ok(0),
            ]
        );
        assert_eq!(
            calls,
            [
                Call::AcquireBuffer { size: 4096 },
                Call::AcquireBuffer { size: 0 },
                Call::Open {
                    path: "/data/sample".to_string(),
                    mode: AccessMode::ReadWrite,
                    flags,
                },
                Call::Close { fd: FD },
                Call::Read {
                    fd: FD,
                    offset: 1024,
                    len: 4096,
                },
                Call::Write {
                    fd: FD,
                    offset: u64::MAX,
                    len: 12,
                },
                Call::SetSize { fd: FD, size: 8192 },
                Call::Stat { fd: FD },
                Call::List { fd: FD, from },
                Call::Rename {
                    from: "/store/a".to_string(),
                    to: "/store/b".to_string(),
                },
                Call::Unlink {
                    path: "/store/a".to_string(),
                    expected: NodeKind::File,
                },
                Call::BackingMemory { fd: FD },
                Call::CreateBacked {
                    path: "/store/shared".to_string(),
                    region: IMPORT_CAP.raw(),
                    size: 123,
                },
                Call::OpenRoot {
                    path: "/window".to_string(),
                },
            ]
        );
    }

    #[test]
    fn path_at_bound_fits_the_frame() {
        let path = "/".to_string() + &"p".repeat(PATH_MAX - 1);
        let (result, calls) =
            with_service(|client| client.open(&path, AccessMode::Read, OpenFlags::NONE));
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
