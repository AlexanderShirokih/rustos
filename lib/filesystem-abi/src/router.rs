//! Контракт маршрутизатора имён: разбор пути в монтирование и выдача scoped-сессии.
//! Клиент передаёт данные напрямую выбранной файловой системе. Таблицей монтирования управляет
//! admin-контракт [`crate::namespace_admin`] на другом порту.

use crate::{error::FileSystemError, limits::PATH_MAX};

ipc::wire_value! {
    /// Сессия, накрывающая оба пути пары, и длина абсолютного пути её корня в байтах, включая
    /// ведущий разделитель. У корня дерева имён длина равна единице. Клиент снимает с обоих
    /// путей `root_len` байт и остаток передаёт этой сессии.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ResolvedPair {
        /// Сессия, открытая на общем каталоге пары либо на корне их монтирования.
        pub session: ipc::wire::Cap<syscall::kind::Port>,
        /// Длина пути, на котором открыта сессия.
        pub root_len: u32,
    }
}

ipc::protocol! {
    /// Маршрутизатор имён. Разбирает путь клиента по таблице монтирований и отдаёт сессию
    /// накрывающей путь файловой системы.
    pub mod namespace {
        name = "Namespace";

        /// Отдаёт свежую сессию файловой системы, открытую с корнем на `path`. Путь разбирается
        /// по самому длинному накрывающему префиксу таблицы, путь без такого префикса -
        /// `NotFound`.
        ///
        /// Capability выдаёт сама файловая система, применяя свою политику допуска. Оттуда же
        /// приходит и отказ установления. Файловая система без ответа в отведённый срок -
        /// `Unresponsive`.
        call fn resolve(
            path: ipc::wire::Str<{ PATH_MAX }>,
        ) -> Result<ipc::wire::Cap<syscall::kind::Port>, FileSystemError>;

        /// Разбирает оба пути по одному снимку таблицы и отдаёт сессию их общего каталога.
        /// Общий каталог выше корня монтирования корнем сессии не становится, и тогда сессия
        /// открывается на корне монтирования. Длину пути выданного корня называет
        /// [`ResolvedPair::root_len`].
        ///
        /// Пути под разными монтированиями - `CrossMount`, путь без накрывающего монтирования -
        /// `NotFound`. В обоих случаях файловая система не вызывается.
        call fn resolve_pair(
            from: ipc::wire::Str<{ PATH_MAX }>,
            to: ipc::wire::Str<{ PATH_MAX }>,
        ) -> Result<ResolvedPair, FileSystemError>;
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

    use super::{FileSystemError, PATH_MAX, ResolvedPair, namespace};

    const SESSION_CAP: Cap<syscall::kind::Port> = cap(0x31);

    const MOUNTS: [&str; 2] = ["/boot", "/data"];

    struct Router {
        resolved: Arc<Mutex<Vec<String>>>,
    }

    impl namespace::Service for Router {
        fn resolve(
            &mut self,
            path: Str<'_, PATH_MAX>,
        ) -> Result<Cap<syscall::kind::Port>, FileSystemError> {
            self.resolved
                .lock()
                .expect("mutex not poisoned")
                .push(path.as_str().to_string());
            if !path.as_str().starts_with("/boot") {
                return Err(FileSystemError::NotFound);
            }
            Ok(SESSION_CAP)
        }

        fn resolve_pair(
            &mut self,
            from: Str<'_, PATH_MAX>,
            to: Str<'_, PATH_MAX>,
        ) -> Result<ResolvedPair, FileSystemError> {
            let mut resolved = self.resolved.lock().expect("mutex not poisoned");
            resolved.push(from.as_str().to_string());
            resolved.push(to.as_str().to_string());
            drop(resolved);

            let first = mount_of(from.as_str()).ok_or(FileSystemError::NotFound)?;
            let second = mount_of(to.as_str()).ok_or(FileSystemError::NotFound)?;
            if first != second {
                return Err(FileSystemError::CrossMount);
            }
            Ok(ResolvedPair {
                session: SESSION_CAP,
                root_len: u32::try_from(first.len()).expect("prefix within the bound"),
            })
        }
    }

    fn mount_of(path: &str) -> Option<&'static str> {
        MOUNTS.into_iter().find(|prefix| {
            path.strip_prefix(prefix)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
        })
    }

    const fn cap<K: syscall::ObjectKind>(raw: u32) -> Cap<K> {
        match NonZeroU32::new(raw) {
            Some(id) => Cap::from_handle(syscall::Handle::from_abi(id)),
            None => panic!("non-zero"),
        }
    }

    fn path(text: &str) -> Str<'_, PATH_MAX> {
        Str::new(text).expect("path within the bound")
    }

    fn with_router<R>(
        calls_expected: usize,
        body: impl FnOnce(&namespace::Client<MockEnd>) -> R,
    ) -> (R, Vec<String>) {
        let (client_end, server_end) = MockEnd::pair();
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let mut router = Router {
            resolved: recorded.clone(),
        };
        thread::scope(|scope| {
            let server = scope.spawn(|| {
                for _ in 0..calls_expected {
                    server_end
                        .wait_readable(ipc::Timeout::INFINITE)
                        .expect("server wait");
                    namespace::dispatch(&mut router, &server_end).expect("dispatch ok");
                }
            });

            let result = body(&namespace::Client::new(client_end));
            server.join().expect("server joins");
            let resolved = recorded.lock().expect("mutex not poisoned").clone();
            (result, resolved)
        })
    }

    #[test]
    fn resolve_carries_the_session_opened_on_the_path() {
        let (mut mounted, resolved) = with_router(1, |client| {
            client.resolve(path("/boot/sample")).expect("call")
        });

        assert_eq!(mounted.value, Ok(SESSION_CAP));
        assert_eq!(mounted.claim(SESSION_CAP), Some(SESSION_CAP));
        assert_eq!(resolved, ["/boot/sample".to_string()]);
    }

    #[test]
    fn resolve_reports_a_path_no_mount_covers() {
        let (mounted, _) = with_router(1, |client| client.resolve(path("/nowhere")).expect("call"));

        assert_eq!(mounted.value, Err(FileSystemError::NotFound));
    }

    #[test]
    fn resolve_pair_carries_the_session_and_the_length_of_the_common_root() {
        let (mut pair, resolved) = with_router(1, |client| {
            client
                .resolve_pair(path("/boot/first"), path("/boot/second"))
                .expect("call")
        });

        let decoded = pair.value.expect("one mount covers the pair");
        assert_eq!(decoded.session, SESSION_CAP);
        assert_eq!(decoded.root_len, 5);
        assert_eq!(pair.claim(decoded.session), Some(SESSION_CAP));
        assert_eq!(
            resolved,
            ["/boot/first".to_string(), "/boot/second".to_string()]
        );
    }

    #[test]
    fn resolve_pair_reports_paths_under_different_mounts() {
        let (pair, _) = with_router(1, |client| {
            client
                .resolve_pair(path("/boot/first"), path("/data/second"))
                .expect("call")
        });

        assert_eq!(pair.value, Err(FileSystemError::CrossMount));
    }

    #[test]
    fn resolve_pair_reports_a_pair_no_mount_covers() {
        let (pair, _) = with_router(1, |client| {
            client
                .resolve_pair(path("/nowhere/first"), path("/boot/second"))
                .expect("call")
        });

        assert_eq!(pair.value, Err(FileSystemError::NotFound));
    }
}
