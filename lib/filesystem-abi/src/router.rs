//! Контракт маршрутизатора имён: разбор пути в монтирование. Данные через маршрутизатор не идут -
//! он участвует только в установлении сессии. Управление таблицей монтирования живёт в admin-
//! контракте [`crate::namespace_admin`] на другом порту.

use crate::{error::FileSystemError, limits::PATH_MAX, reply::Mounted};

ipc::protocol! {
    /// Маршрутизатор имён: выдаёт клиентам сессии файловых систем по пути.
    pub mod namespace {
        name = "Namespace";

        /// Отдаёт сессию монтирования, накрывающего `path`, вместе с длиной его префикса.
        /// Путь, не накрытый ни одним монтированием, - `NotFound`.
        ///
        /// Capability выдаёт сама файловая система, применяя свою политику допуска: от неё же
        /// приходит и отказ установления.
        call fn resolve(path: ipc::wire::Str<{ PATH_MAX }>) -> Result<Mounted, FileSystemError>;
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

    use super::{FileSystemError, Mounted, PATH_MAX, namespace};

    const SESSION_CAP: Cap = cap(0x31);
    const BOOT_PREFIX_LEN: u32 = 5;

    struct Router {
        resolved: Arc<Mutex<Vec<String>>>,
    }

    impl namespace::Service for Router {
        fn resolve(&mut self, path: Str<'_, PATH_MAX>) -> Result<Mounted, FileSystemError> {
            self.resolved
                .lock()
                .expect("mutex not poisoned")
                .push(path.as_str().to_string());
            if !path.as_str().starts_with("/boot") {
                return Err(FileSystemError::NotFound);
            }
            Ok(Mounted {
                session: SESSION_CAP,
                prefix_len: BOOT_PREFIX_LEN,
            })
        }
    }

    const fn cap(raw: u32) -> Cap {
        Cap::from_raw(NonZeroU32::new(raw).expect("non-zero"))
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
    fn resolve_carries_the_session_and_its_prefix_in_one_call() {
        let (mounted, resolved) = with_router(1, |client| {
            client.resolve(path("/boot/sample")).expect("call")
        });

        assert_eq!(
            mounted,
            Ok(Mounted {
                session: SESSION_CAP,
                prefix_len: BOOT_PREFIX_LEN,
            })
        );
        assert_eq!(resolved, ["/boot/sample".to_string()]);
    }

    #[test]
    fn resolve_reports_a_path_no_mount_covers() {
        let (mounted, _) = with_router(1, |client| client.resolve(path("/nowhere")).expect("call"));

        assert_eq!(mounted, Err(FileSystemError::NotFound));
    }
}
