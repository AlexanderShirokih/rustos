//! Обработчик области экспорта `FileSystem`. Сессия с пустой областью открыта на корне файловой
//! системы, сессия с непустой областью - на каталоге по пути области.

use component_abi::{SESSION_SCOPE_MAX, SessionError, component_startup as startup_contract};
use component_startup::{MintedSession, mint_session, serve_export};
use filesystem_abi::{FileSystemError, PATH_MAX, file_system};
use kernel_objects::Port;
use runtime::{PortTransport, log_fmt, process_exit};
use syscall::{Handle, kind};

use crate::{FAILED_EXIT_CODE, Shared, error::no_space};

const _: () = assert!(
    // Файловая система разбирает область заявки как путь.
    SESSION_SCOPE_MAX == PATH_MAX,
    "session scope differs from the path bound"
);

pub(crate) fn serve(start: Handle<kind::Port>, shared: &'static Shared) -> u32 {
    let parent = startup_contract::Client::new(PortTransport::new(start));
    let failure =
        serve_export::<file_system::Protocol>(&parent, |scope| open_session(shared, scope));
    log_fmt(format_args!("export stopped: {failure:?}"));
    process_exit(FAILED_EXIT_CODE)
}

pub(crate) fn open_session(shared: &Shared, scope: &str) -> Result<Port, SessionError> {
    let MintedSession { badge, port } = mint_session(&shared.service, &shared.next_badge)
        .map_err(|error| refused(no_space(error)))?;

    // Запись сессии создаётся до передачи её capability клиенту.
    let mut state = shared.state.lock();
    let opened = if scope.is_empty() {
        state.open_session(badge.get())
    } else {
        state.open_session_at(badge.get(), scope)
    };
    drop(state);
    opened.map_err(refused)?;

    Ok(port)
}

fn refused(error: FileSystemError) -> SessionError {
    match error {
        FileSystemError::NoSpace => SessionError::NoSpace,
        _ => SessionError::Refused,
    }
}
