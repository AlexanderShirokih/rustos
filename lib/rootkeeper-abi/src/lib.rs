//! Контракт rootkeeper с компонентами загрузочной композиции.

#![cfg_attr(not(test), no_std)]

use filesystem_abi::{FileSystemError, PATH_MAX};

ipc::protocol! {
    /// Операции supervisor, доступные компоненту через слот `supervisor`.
    pub mod rootkeeper {
        name = "Rootkeeper";

        /// Отдаёт сессию файловой системы, которую обслуживает уже поднятый компонент
        /// плана с программой `program`.
        call fn file_system(
            program: ipc::wire::Str<{ PATH_MAX }>,
        ) -> Result<ipc::wire::Cap, FileSystemError>;
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU32;

    use filesystem_abi::{FileSystemError, PATH_MAX};
    use ipc::wire::{Cap, Str};
    use ipc_test::MockEnd;

    use super::rootkeeper;

    const SESSION: Cap = Cap::from_raw(NonZeroU32::new(0x31).expect("non-zero"));

    #[derive(Default)]
    struct Broker;

    impl rootkeeper::Service for Broker {
        fn file_system(&mut self, program: Str<'_, PATH_MAX>) -> Result<Cap, FileSystemError> {
            if program.as_str() == "/boot/filesystem-imagefs" {
                Ok(SESSION)
            } else {
                Err(FileSystemError::NotFound)
            }
        }
    }

    #[test]
    fn file_system_session_is_selected_by_plan_program() {
        let (client_end, server_end) = MockEnd::pair();
        let mut broker = Broker;
        std::thread::scope(|scope| {
            let server = scope.spawn(|| {
                for _ in 0..2 {
                    ipc::Transport::wait_readable(&server_end, ipc::Timeout::INFINITE)
                        .expect("server wait");
                    rootkeeper::dispatch(&mut broker, &server_end).expect("dispatch ok");
                }
            });
            let client = rootkeeper::Client::new(client_end);
            assert_eq!(
                client
                    .file_system(Str::new("/boot/filesystem-imagefs").expect("path"))
                    .expect("call"),
                Ok(SESSION)
            );
            assert_eq!(
                client
                    .file_system(Str::new("/boot/missing").expect("path"))
                    .expect("call"),
                Err(FileSystemError::NotFound)
            );
            server.join().expect("server joins");
        });
    }
}
