//! Поток монтирования файловой системы в дерево имён по сессии администратора `namespace`.

use component_abi::component_startup as startup_contract;
use component_startup::session;
use filesystem_abi::{file_system, namespace_admin};
use ipc::wire::IpcError;
use kernel_objects::Session;
use runtime::{PortTransport, log_fmt, log_line, process_exit};
use syscall::{Handle, kind};

use crate::{FAILED_EXIT_CODE, Shared, export::open_session};

const NAMESPACE_REQUIREMENT: &str = "namespace";

pub(crate) fn serve(
    start: Handle<kind::Port>,
    namespace: Session<namespace_admin::Protocol>,
    shared: &'static Shared,
) -> u32 {
    // Вызов `mount` и повторный запрос `namespace` ждут без срока.
    let parent = startup_contract::Client::new(PortTransport::new(start));
    let mut admin = namespace;
    loop {
        let filesystem = match open_session(shared, "") {
            Ok(port) => Session::<file_system::Protocol>::from_handle(port),
            Err(refusal) => {
                log_fmt(format_args!("mount session refused: {refusal:?}"));
                process_exit(FAILED_EXIT_CODE)
            }
        };
        match admin.client().mount(filesystem) {
            // Маршрутизатор ушёл, и новая incarnation получит монтирование по новой сессии.
            Err(IpcError::PeerClosed) => {}
            Ok(Ok(())) => {
                log_line("mount ended");
                return 0;
            }
            Ok(Err(refusal)) => {
                log_fmt(format_args!("mount refused: {refusal:?}"));
                process_exit(FAILED_EXIT_CODE)
            }
            Err(error) => {
                log_fmt(format_args!("mount failed: {error:?}"));
                process_exit(FAILED_EXIT_CODE)
            }
        }
        let Some(renewed) = renew(&parent) else {
            return 0;
        };
        admin = renewed;
    }
}

fn renew(
    parent: &startup_contract::Client<PortTransport>,
) -> Option<Session<namespace_admin::Protocol>> {
    loop {
        match session::<namespace_admin::Protocol>(parent, NAMESPACE_REQUIREMENT) {
            Ok(Ok(admin)) => return Some(admin),
            Err(IpcError::PeerClosed) => {}
            // Файловая система без монтирования продолжает обслуживать импортёров экспорта.
            Ok(Err(refusal)) => {
                log_fmt(format_args!("namespace unavailable: {refusal:?}"));
                return None;
            }
            Err(error) => {
                log_fmt(format_args!("namespace request failed: {error:?}"));
                process_exit(FAILED_EXIT_CODE)
            }
        }
    }
}
