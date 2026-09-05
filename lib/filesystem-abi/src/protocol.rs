//! Контракт файлового сервиса: команды над деревом имён и узлами. Данные переменной длины идут
//! через буфер данных сессии, кадр несёт пути, длины и счётчики.
//!
//! Порт равен монтированию: путь отсчитывается от корня монтирования, а о своём префиксе и о
//! соседях сервер не знает. Префикс снимает клиент по длине, которую отдал маршрутизатор имён.
//!
//! Порядок операций закреплён за вызовом, сессия его не добавляет: несколько незавершённых вызовов
//! применяются в любом порядке, и исход их пересечения на одном узле контракт не определяет.
//! Клиент, которому нужен порядок, держит на сессии один незавершённый вызов. Ослабление идёт от
//! приёма несколькими потоками сервера. Значение закрытого дескриптора при этом не
//! переназначается другому узлу: отставший вызов по нему получает `BadDescriptor`.

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

        /// Выделяет сессии буфер данных на `size` байт и отдаёт RW-capability на него.
        call fn acquire_buffer(size: u32) -> Result<ipc::wire::Cap, FileSystemError>;

        /// Открывает узел по пути внутри монтирования, закрепляя за дескриптором его
        /// идентичность. `mode` задаёт режим доступа дескриптора, `flags` - действия над
        /// деревом имён.
        call fn open(
            path: ipc::wire::Str<{ PATH_MAX }>,
            mode: AccessMode,
            flags: OpenFlags,
        ) -> Result<Opened, FileSystemError>;

        /// Закрывает дескриптор сессии.
        call fn close(fd: NodeDescriptor) -> Result<u32, FileSystemError>;

        /// Кладёт в начало буфера сессии до `len` байт узла со смещения `offset`. Возвращает
        /// число прочитанных байт.
        call fn read(fd: NodeDescriptor, offset: u64, len: u32) -> Result<u32, FileSystemError>;

        /// Пишет в узел `len` байт из начала буфера сессии по смещению `offset`. Значение
        /// `offset == u64::MAX` - дозапись в конец, неделимая между сессиями.
        call fn write(
            fd: NodeDescriptor,
            offset: u64,
            len: u32,
        ) -> Result<Written, FileSystemError>;

        /// Задаёт длину узла: усечение либо предварительное расширение.
        call fn set_size(fd: NodeDescriptor, size: u64) -> Result<u32, FileSystemError>;

        /// Отдаёт метаданные узла, закреплённого за дескриптором.
        call fn stat(fd: NodeDescriptor) -> Result<NodeInfo, FileSystemError>;

        /// Заполняет буфер сессии записями каталога начиная с позиции `from`. Перечисление
        /// продолжает вызов с `from` из [`Listed::next`].
        call fn list(fd: NodeDescriptor, from: DirCursor) -> Result<Listed, FileSystemError>;

        /// Переименовывает узел. Переход через границу монтирования - `CrossMount`.
        call fn rename(
            from: ipc::wire::Str<{ PATH_MAX }>,
            to: ipc::wire::Str<{ PATH_MAX }>,
        ) -> Result<u32, FileSystemError>;

        /// Удаляет узел по пути, вид которого совпал с `expected`. Ожидание файла при
        /// фактическом каталоге - `IsADirectory`, ожидание каталога при фактическом файле -
        /// `NotADirectory`. Каталог с записями - `NotEmpty`.
        call fn unlink(
            path: ipc::wire::Str<{ PATH_MAX }>,
            expected: NodeKind,
        ) -> Result<u32, FileSystemError>;

        /// Заводит файл под `path` поверх переданного RW-региона без копирования. `size` задаёт
        /// начальную длину и не может превышать ёмкость региона.
        call fn create_backed(
            path: ipc::wire::Str<{ PATH_MAX }>,
            region: Cap,
            size: u64,
        ) -> Result<u32, FileSystemError>;

        /// Отдаёт capability на регион с содержимым узла. Права выводятся из режима
        /// дескриптора. Бекенд без такого региона отвечает `NotSupported`.
        call fn backing_memory(fd: NodeDescriptor) -> Result<ipc::wire::Cap, FileSystemError>;

        /// Отдаёт capability свежей сессии с каталогом `path` в роли корня: окно только
        /// сужается. Путь не каталога - `NotADirectory`, путь за `PATH_MAX` вместе с окном -
        /// `InvalidPath`.
        call fn open_root(
            path: ipc::wire::Str<{ PATH_MAX }>,
        ) -> Result<ipc::wire::Cap, FileSystemError>;
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU32;
    use std::{
        string::{String, ToString},
        sync::{Arc, Mutex},
        thread,
        vec::Vec,
    };

    use ipc::{
        Transport,
        wire::{Cap, Str},
    };
    use ipc_test::MockEnd;

    use super::{
        AccessMode, DirCursor, FileSystemError, Listed, NodeDescriptor, NodeInfo, NodeKind,
        OpenFlags, Opened, PATH_MAX, Written, file_system,
    };

    const BUFFER_CAP: Cap = cap(0x11);
    const BACKING_CAP: Cap = cap(0x22);
    const SCOPED_CAP: Cap = cap(0x33);
    const IMPORT_CAP: Cap = cap(0x44);
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

    impl file_system::Service for Recorder {
        fn acquire_buffer(&mut self, size: u32) -> Result<Cap, FileSystemError> {
            self.record(Call::AcquireBuffer { size });
            if size == 0 {
                return Err(FileSystemError::NoSpace);
            }
            Ok(BUFFER_CAP)
        }

        fn open(
            &mut self,
            path: Str<'_, { PATH_MAX }>,
            mode: AccessMode,
            flags: OpenFlags,
        ) -> Result<Opened, FileSystemError> {
            self.record(Call::Open {
                path: path.as_str().to_string(),
                mode,
                flags,
            });
            Ok(OPENED)
        }

        fn close(&mut self, fd: NodeDescriptor) -> Result<u32, FileSystemError> {
            self.record(Call::Close { fd });
            Ok(DONE)
        }

        fn read(
            &mut self,
            fd: NodeDescriptor,
            offset: u64,
            len: u32,
        ) -> Result<u32, FileSystemError> {
            self.record(Call::Read { fd, offset, len });
            Ok(READ_COUNT)
        }

        fn write(
            &mut self,
            fd: NodeDescriptor,
            offset: u64,
            len: u32,
        ) -> Result<Written, FileSystemError> {
            self.record(Call::Write { fd, offset, len });
            Ok(WRITTEN)
        }

        fn set_size(&mut self, fd: NodeDescriptor, size: u64) -> Result<u32, FileSystemError> {
            self.record(Call::SetSize { fd, size });
            Ok(DONE)
        }

        fn stat(&mut self, fd: NodeDescriptor) -> Result<NodeInfo, FileSystemError> {
            self.record(Call::Stat { fd });
            Ok(NODE_INFO)
        }

        fn list(&mut self, fd: NodeDescriptor, from: DirCursor) -> Result<Listed, FileSystemError> {
            self.record(Call::List { fd, from });
            Ok(LISTED)
        }

        fn rename(
            &mut self,
            from: Str<'_, { PATH_MAX }>,
            to: Str<'_, { PATH_MAX }>,
        ) -> Result<u32, FileSystemError> {
            self.record(Call::Rename {
                from: from.as_str().to_string(),
                to: to.as_str().to_string(),
            });
            Ok(DONE)
        }

        fn unlink(
            &mut self,
            path: Str<'_, { PATH_MAX }>,
            expected: NodeKind,
        ) -> Result<u32, FileSystemError> {
            self.record(Call::Unlink {
                path: path.as_str().to_string(),
                expected,
            });
            Ok(DONE)
        }

        fn create_backed(
            &mut self,
            path: Str<'_, { PATH_MAX }>,
            region: Cap,
            size: u64,
        ) -> Result<u32, FileSystemError> {
            self.record(Call::CreateBacked {
                path: path.as_str().to_string(),
                region: region.raw().get(),
                size,
            });
            Ok(DONE)
        }

        fn backing_memory(&mut self, fd: NodeDescriptor) -> Result<Cap, FileSystemError> {
            self.record(Call::BackingMemory { fd });
            Ok(BACKING_CAP)
        }

        fn open_root(&mut self, path: Str<'_, { PATH_MAX }>) -> Result<Cap, FileSystemError> {
            self.record(Call::OpenRoot {
                path: path.as_str().to_string(),
            });
            Ok(SCOPED_CAP)
        }
    }

    const fn cap(raw: u32) -> Cap {
        Cap::from_raw(NonZeroU32::new(raw).expect("non-zero"))
    }

    fn with_service<R>(body: impl FnOnce(&file_system::Client<MockEnd>) -> R) -> (R, Vec<Call>) {
        let (client_end, server_end) = MockEnd::pair();
        let client = file_system::Client::new(client_end);
        let calls = Arc::new(Mutex::new(Vec::new()));
        let result = thread::scope(|scope| {
            let mut service = Recorder {
                calls: Arc::clone(&calls),
            };
            scope.spawn(move || {
                while server_end.wait_readable(ipc::Timeout::INFINITE).is_ok() {
                    if file_system::dispatch(&mut service, &server_end).is_err() {
                        break;
                    }
                }
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

    #[test]
    fn acquire_buffer_round_trip() {
        let (mut result, calls) =
            with_service(|client| client.acquire_buffer(4096).expect("call ok"));
        assert_eq!(result.value, Ok(BUFFER_CAP));
        assert_eq!(result.claim(BUFFER_CAP), Some(BUFFER_CAP));
        // Повторный запрос той же capability ответ уже не находит.
        assert_eq!(result.claim(BUFFER_CAP), None);
        assert_eq!(calls, [Call::AcquireBuffer { size: 4096 }]);
    }

    #[test]
    fn domain_error_round_trips_as_fs_error() {
        let (result, calls) = with_service(|client| client.acquire_buffer(0).expect("call ok"));
        assert_eq!(result.value, Err(FileSystemError::NoSpace));
        assert_eq!(calls, [Call::AcquireBuffer { size: 0 }]);
    }

    #[test]
    fn open_round_trip() {
        let flags = OpenFlags::CREATE | OpenFlags::EXCLUSIVE;
        let (result, calls) = with_service(|client| {
            client
                .open(
                    Str::new("/data/sample").expect("path within bound"),
                    AccessMode::ReadWrite,
                    flags,
                )
                .expect("call ok")
        });
        assert_eq!(result.value, Ok(OPENED));
        assert_eq!(
            calls,
            [Call::Open {
                path: "/data/sample".to_string(),
                mode: AccessMode::ReadWrite,
                flags,
            }]
        );
    }

    #[test]
    fn path_at_bound_fits_the_frame() {
        let path = "/".to_string() + &"p".repeat(PATH_MAX - 1);
        let (result, calls) = with_service(|client| {
            client
                .open(
                    Str::new(&path).expect("path at bound"),
                    AccessMode::Read,
                    OpenFlags::NONE,
                )
                .expect("call ok")
        });
        assert_eq!(result.value, Ok(OPENED));
        assert_eq!(
            calls,
            [Call::Open {
                path,
                mode: AccessMode::Read,
                flags: OpenFlags::NONE,
            }]
        );
    }

    #[test]
    fn close_round_trip() {
        let (result, calls) = with_service(|client| client.close(FD).expect("call ok"));
        assert_eq!(result.value, Ok(DONE));
        assert_eq!(calls, [Call::Close { fd: FD }]);
    }

    #[test]
    fn read_round_trip() {
        let (result, calls) = with_service(|client| client.read(FD, 1024, 4096).expect("call ok"));
        assert_eq!(result.value, Ok(READ_COUNT));
        assert_eq!(
            calls,
            [Call::Read {
                fd: FD,
                offset: 1024,
                len: 4096,
            }]
        );
    }

    #[test]
    fn write_round_trip() {
        let (result, calls) =
            with_service(|client| client.write(FD, u64::MAX, 12).expect("call ok"));
        assert_eq!(result.value, Ok(WRITTEN));
        assert_eq!(
            calls,
            [Call::Write {
                fd: FD,
                offset: u64::MAX,
                len: 12,
            }]
        );
    }

    #[test]
    fn set_size_round_trip() {
        let (result, calls) = with_service(|client| client.set_size(FD, 8192).expect("call ok"));
        assert_eq!(result.value, Ok(DONE));
        assert_eq!(calls, [Call::SetSize { fd: FD, size: 8192 }]);
    }

    #[test]
    fn stat_round_trip() {
        let (result, calls) = with_service(|client| client.stat(FD).expect("call ok"));
        assert_eq!(result.value, Ok(NODE_INFO));
        assert_eq!(calls, [Call::Stat { fd: FD }]);
    }

    #[test]
    fn list_round_trip() {
        let from = DirCursor::from_raw(2);
        let (result, calls) = with_service(|client| client.list(FD, from).expect("call ok"));
        assert_eq!(result.value, Ok(LISTED));
        assert_eq!(calls, [Call::List { fd: FD, from }]);
    }

    #[test]
    fn rename_round_trip() {
        let (result, calls) = with_service(|client| {
            client
                .rename(
                    Str::new("/store/a").expect("path within bound"),
                    Str::new("/store/b").expect("path within bound"),
                )
                .expect("call ok")
        });
        assert_eq!(result.value, Ok(DONE));
        assert_eq!(
            calls,
            [Call::Rename {
                from: "/store/a".to_string(),
                to: "/store/b".to_string(),
            }]
        );
    }

    #[test]
    fn unlink_round_trip() {
        let (result, calls) = with_service(|client| {
            client
                .unlink(
                    Str::new("/store/a").expect("path within bound"),
                    NodeKind::File,
                )
                .expect("call ok")
        });
        assert_eq!(result.value, Ok(DONE));
        assert_eq!(
            calls,
            [Call::Unlink {
                path: "/store/a".to_string(),
                expected: NodeKind::File,
            }]
        );
    }

    #[test]
    fn backing_memory_round_trip() {
        let (mut result, calls) =
            with_service(|client| client.backing_memory(FD).expect("call ok"));
        assert_eq!(result.value, Ok(BACKING_CAP));
        assert_eq!(result.claim(BACKING_CAP), Some(BACKING_CAP));
        assert_eq!(result.claim(BACKING_CAP), None);
        assert_eq!(calls, [Call::BackingMemory { fd: FD }]);
    }

    #[test]
    fn create_backed_round_trip() {
        let (result, calls) = with_service(|client| {
            client
                .create_backed(
                    Str::new("/store/shared").expect("path within bound"),
                    IMPORT_CAP,
                    123,
                )
                .expect("call ok")
        });
        assert_eq!(result.value, Ok(DONE));
        assert_eq!(
            calls,
            [Call::CreateBacked {
                path: "/store/shared".to_string(),
                region: IMPORT_CAP.raw().get(),
                size: 123,
            }]
        );
    }

    #[test]
    fn open_root_round_trip() {
        let (mut result, calls) = with_service(|client| {
            client
                .open_root(Str::new("/window").expect("path within bound"))
                .expect("call ok")
        });
        assert_eq!(result.value, Ok(SCOPED_CAP));
        assert_eq!(result.claim(SCOPED_CAP), Some(SCOPED_CAP));
        assert_eq!(result.claim(SCOPED_CAP), None);
        assert_eq!(
            calls,
            [Call::OpenRoot {
                path: "/window".to_string(),
            }]
        );
    }
}
