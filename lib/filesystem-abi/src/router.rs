//! Контракт маршрутизатора имён. Контракт [`crate::file_system_mount`] на другом порту создаёт
//! монтирования.

use crate::{error::FileSystemError, limits::PATH_MAX};

ipc::wire_value! {
    /// Сессия, накрывающая оба пути пары. Клиент снимает с обоих путей `root_len` байт и остаток
    /// передаёт этой сессии.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ResolvedPair<S = ipc::wire::Session<crate::file_system::Protocol>> {
        /// Сессия, открытая на общем каталоге пары либо на корне их монтирования.
        pub session: S,
        /// Длина пути, на котором открыта сессия, с ведущим разделителем. Путь отсчитан от
        /// области клиента.
        pub root_len: u32,
    }
}

ipc::protocol! {
    /// Маршрутизатор имён. Разбирает путь клиента по монтированиям на области сессии и под ней.
    pub mod namespace {
        name = "Namespace";

        /// Выдаёт сессию маршрутизатора с корнем на `path` внутри текущей сессии.
        call fn open_root(path: ipc::wire::Str<{ PATH_MAX }>)
            -> own Result<ipc::wire::Session<crate::namespace::Protocol>, FileSystemError>;

        /// Отдаёт свежую сессию файловой системы с корнем на `path`. Путь отсчитывается от области
        /// сессии и разбирается по самому длинному префиксу монтирований. Пока путь не накрыт,
        /// запрос ждёт монтирования; путь выше монтирования - `NotFound`.
        ///
        /// Capability выдаёт сама файловая система, применяя свою политику допуска. Оттуда же
        /// приходит и отказ установления. Файловая система без ответа в отведённый срок -
        /// `Unresponsive`. Маршрутизатор, которому нечем удержать запрос, отвечает `NoSpace`.
        call(defer) fn resolve(
            path: ipc::wire::Str<{ PATH_MAX }>,
        ) -> own Result<ipc::wire::Session<crate::file_system::Protocol>, FileSystemError>;

        /// Разбирает оба пути по одному снимку монтирований и отдаёт сессию их общего каталога, не
        /// выше корня монтирования. Пока хотя бы один путь не накрыт, запрос ждёт. Пути под
        /// разными монтированиями - `CrossMount`. Путь выше монтирования - `NotFound`.
        call(defer) fn resolve_pair(
            from: ipc::wire::Str<{ PATH_MAX }>,
            to: ipc::wire::Str<{ PATH_MAX }>,
        ) -> own Result<ResolvedPair, FileSystemError>;
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

    use ipc::wire::{Session, Str};
    use ipc_test::{MockHandle, MockReply, MockResources};
    use syscall::kind;

    use super::{FileSystemError, PATH_MAX, ResolvedPair, namespace};
    use crate::file_system;

    type FileSystemSession = Session<file_system::Protocol, MockHandle<kind::Port>>;

    const SESSION: FileSystemSession = Session::from_handle(MockHandle::new(0x31));

    const MOUNTS: [&str; 2] = ["/boot", "/data"];

    struct Router {
        resolved: Arc<Mutex<Vec<String>>>,
    }

    impl namespace::Service<MockResources> for Router {
        fn open_root(
            &mut self,
            _frame: &MockResources,
            _path: Str<'_, PATH_MAX>,
        ) -> impl ipc::IntoWireValue<
            MockResources,
            Read = Result<Session<namespace::Protocol>, FileSystemError>,
        > {
            Err::<Session<namespace::Protocol, MockHandle<kind::Port>>, _>(
                FileSystemError::NoSession,
            )
        }
        fn resolve(
            &mut self,
            _frame: &MockResources,
            path: Str<'_, PATH_MAX>,
            reply: namespace::resolve::Reply<MockReply>,
        ) {
            self.resolved
                .lock()
                .expect("mutex not poisoned")
                .push(path.as_str().to_string());
            let session = if path.as_str().starts_with("/boot") {
                Ok(SESSION)
            } else {
                Err(FileSystemError::NotFound)
            };
            reply.send(session).expect("the caller waits");
        }

        fn resolve_pair(
            &mut self,
            _frame: &MockResources,
            from: Str<'_, PATH_MAX>,
            to: Str<'_, PATH_MAX>,
            reply: namespace::resolve_pair::Reply<MockReply>,
        ) {
            let mut resolved = self.resolved.lock().expect("mutex not poisoned");
            resolved.push(from.as_str().to_string());
            resolved.push(to.as_str().to_string());
            drop(resolved);

            reply
                .send(pair_of(from.as_str(), to.as_str()))
                .expect("the caller waits");
        }
    }

    fn pair_of(from: &str, to: &str) -> Result<ResolvedPair<FileSystemSession>, FileSystemError> {
        let first = mount_of(from).ok_or(FileSystemError::NotFound)?;
        let second = mount_of(to).ok_or(FileSystemError::NotFound)?;
        if first != second {
            return Err(FileSystemError::CrossMount);
        }
        Ok(ResolvedPair {
            session: SESSION,
            root_len: u32::try_from(first.len()).expect("prefix within the bound"),
        })
    }

    fn mount_of(path: &str) -> Option<&'static str> {
        MOUNTS.into_iter().find(|prefix| {
            path.strip_prefix(prefix)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
        })
    }

    fn path(text: &str) -> Str<'_, PATH_MAX> {
        Str::new(text).expect("path within the bound")
    }

    fn with_router<R>(
        calls_expected: usize,
        body: impl FnOnce(&namespace::Client<ipc_test::MockChannel>) -> R,
    ) -> (R, Vec<String>) {
        let (client_end, server_end) = ipc_test::pair();
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let mut router = Router {
            resolved: recorded.clone(),
        };
        thread::scope(|scope| {
            let server = scope.spawn(|| {
                for _ in 0..calls_expected {
                    server_end
                        .serve(|frame| namespace::dispatch(&mut router, frame))
                        .expect("dispatch ok");
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
        let (mounted, resolved) = with_router(1, |client| client.resolve(path("/boot/sample")));

        assert_eq!(mounted, Ok(SESSION));
        assert_eq!(resolved, ["/boot/sample".to_string()]);
    }

    #[test]
    fn resolve_carries_a_refusal_of_the_file_system() {
        let (mounted, _) = with_router(1, |client| client.resolve(path("/nowhere")));

        assert_eq!(
            mounted,
            Err(ipc::CallError::Remote(FileSystemError::NotFound))
        );
    }

    #[test]
    fn resolve_pair_carries_the_session_and_the_length_of_the_common_root() {
        let (pair, resolved) = with_router(1, |client| {
            client.resolve_pair(path("/boot/first"), path("/boot/second"))
        });

        let decoded = pair.expect("one mount covers the pair");
        assert_eq!(decoded.session, SESSION);
        assert_eq!(decoded.root_len, 5);
        assert_eq!(
            resolved,
            ["/boot/first".to_string(), "/boot/second".to_string()]
        );
    }

    #[test]
    fn resolve_pair_reports_paths_under_different_mounts() {
        let (pair, _) = with_router(1, |client| {
            client.resolve_pair(path("/boot/first"), path("/data/second"))
        });

        assert_eq!(
            pair,
            Err(ipc::CallError::Remote(FileSystemError::CrossMount))
        );
    }

    #[test]
    fn resolve_pair_carries_a_refusal_of_the_file_system() {
        let (pair, _) = with_router(1, |client| {
            client.resolve_pair(path("/nowhere/first"), path("/boot/second"))
        });

        assert_eq!(pair, Err(ipc::CallError::Remote(FileSystemError::NotFound)));
    }
}
