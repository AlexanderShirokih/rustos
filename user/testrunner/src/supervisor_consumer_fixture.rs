#![no_std]
#![no_main]
#![allow(unsafe_code)]

extern crate alloc;

use alloc::{format, vec::Vec};
use core::panic::PanicInfo;

use component_abi::component_manifest;
use filesystem_abi::{AccessMode, NodeKind, OpenFlags, file_system};
use ipc::wire::{Bytes, Str};
use runtime::{
    MemoryRegion, OwnedHandle, PortTransport, Timeout, install_log, log_fmt, thread_exit,
};
use supervisor_abi::{ExtendError, INSTANCE_NAME_MAX, PLAN_RECORD_MAX, supervisor};
use syscall::Handle;
use userland_image::{COMPONENT_PROGRAM_NAME, ComponentSpec, OnExhausted, encode_plan};

const LOG_TAG: &str = "extender";

const CALL_TIMEOUT: Timeout = Timeout::from_secs(15);

const RAM_FS_PROGRAM: &str = "/boot/filesystem-ramfs";
const DEFINED_NAME: &str = "extended-ramfs";
const STAGE_ZERO_NAME: &str = "filesystem-imagefs";

const DEFINED_MEMORY_PAGES: u64 = 1_024;
const DEFINED_STARTUP_TIMEOUT: Timeout = Timeout::from_secs(2);

const CREATED_FILE: &str = "/raised-by-the-transaction";

const OK_EXIT_CODE: u64 = 0;
const NO_STARTUP_EXIT_CODE: u64 = 1;
const NO_PROGRAM_EXIT_CODE: u64 = 3;
const REFUSED_EXIT_CODE: u64 = 4;
const UNRESPONSIVE_EXIT_CODE: u64 = 5;
const PANIC_EXIT_CODE: u64 = 6;
const LEAKED_EXIT_CODE: u64 = 7;

component_manifest! {
    struct SupervisorConsumerStartup {
        slots: {
            files: service(protocol = "FileSystem", instance = "filesystem-imagefs"),
            system: supervisor,
        },
    }
}

/// Программа-потребитель транзакции расширения: берёт регионом байты программы файловой
/// системы, определяет её компонентом композиции, поднимает набор и работает с сессией
/// поднятого компонента. `startup` приходит в x0 как HandleId одноразового startup-порта.
#[unsafe(no_mangle)]
pub extern "C" fn _start(startup: Handle) -> ! {
    let startup = OwnedHandle::from_handle(startup);
    let Ok(Some(started)) =
        SupervisorConsumerStartup::receive(&PortTransport::server(startup.borrow()))
    else {
        thread_exit(NO_STARTUP_EXIT_CODE);
    };
    drop(startup);
    install_log(OwnedHandle::adopt(started.log), LOG_TAG);
    drop(OwnedHandle::adopt(started.control));
    drop(OwnedHandle::adopt(started.service));

    let files = OwnedHandle::adopt(started.files);
    let system = OwnedHandle::adopt(started.system);
    thread_exit(raise(
        &file_system::Client::new(PortTransport::client(&files)).with_wait(CALL_TIMEOUT),
        &supervisor::Client::new(PortTransport::client(&system)).with_wait(CALL_TIMEOUT),
    ))
}

fn raise(
    files: &file_system::Client<PortTransport>,
    system: &supervisor::Client<PortTransport>,
) -> u64 {
    let Some(program) = program_region(files) else {
        return NO_PROGRAM_EXIT_CODE;
    };
    let Some(record) = defined_record() else {
        return NO_PROGRAM_EXIT_CODE;
    };
    let Some(record) = Bytes::<PLAN_RECORD_MAX>::new(&record) else {
        return NO_PROGRAM_EXIT_CODE;
    };

    if let Some(code) = commit(system, record, program) {
        return code;
    }
    let Some(name) = Str::<INSTANCE_NAME_MAX>::new(DEFINED_NAME) else {
        return REFUSED_EXIT_CODE;
    };
    let Some(stage_zero) = Str::<INSTANCE_NAME_MAX>::new(STAGE_ZERO_NAME) else {
        return REFUSED_EXIT_CODE;
    };
    // Композиция ступени 0 через транзакцию не достаётся: её компоненты вызыватель не
    // определял.
    match system.session(stage_zero) {
        Ok(Err(ExtendError::NotFound)) => {}
        Ok(Ok(_)) => return LEAKED_EXIT_CODE,
        Ok(Err(error)) => {
            log_fmt(format_args!("stage zero session refused as {error:?}"));
            return LEAKED_EXIT_CODE;
        }
        Err(error) => {
            log_fmt(format_args!("supervisor port unreachable: {error:?}"));
            return UNRESPONSIVE_EXIT_CODE;
        }
    }

    let session = match system.session(name) {
        Ok(Ok(session)) => OwnedHandle::adopt(session),
        Ok(Err(error)) => {
            log_fmt(format_args!(
                "session of the defined component refused: {error:?}"
            ));
            return REFUSED_EXIT_CODE;
        }
        Err(error) => {
            log_fmt(format_args!("supervisor port unreachable: {error:?}"));
            return UNRESPONSIVE_EXIT_CODE;
        }
    };
    serves(&file_system::Client::new(PortTransport::client(&session)).with_wait(CALL_TIMEOUT))
}

fn commit(
    system: &supervisor::Client<PortTransport>,
    record: Bytes<'_, PLAN_RECORD_MAX>,
    program: MemoryRegion,
) -> Option<u64> {
    let opened = system.begin();
    let defined = opened.and_then(|_| system.define(record, program.into_handle()));
    let committed = defined.and_then(|_| system.commit());
    match committed {
        Ok(Ok(_)) => None,
        Ok(Err(error)) => {
            log_fmt(format_args!("transaction refused: {error:?}"));
            Some(REFUSED_EXIT_CODE)
        }
        Err(error) => {
            log_fmt(format_args!("supervisor port unreachable: {error:?}"));
            Some(UNRESPONSIVE_EXIT_CODE)
        }
    }
}

fn program_region(files: &file_system::Client<PortTransport>) -> Option<MemoryRegion> {
    let record = format!("{RAM_FS_PROGRAM}/{COMPONENT_PROGRAM_NAME}");
    let path = Str::new(&record)?;
    let opened = files
        .open(path, AccessMode::Read, OpenFlags::NONE)
        .ok()?
        .ok()?;
    if opened.kind != NodeKind::File {
        return None;
    }
    let region = files.backing_memory(opened.fd).ok()?.ok()?;
    Some(MemoryRegion::adopt(region))
}

fn defined_record() -> Option<Vec<u8>> {
    encode_plan(&[ComponentSpec {
        executable: RAM_FS_PROGRAM,
        name: DEFINED_NAME,
        slots: &[],
        after: &[],
        memory_pages: DEFINED_MEMORY_PAGES,
        startup_timeout: DEFINED_STARTUP_TIMEOUT,
        launches: 1,
        on_exhausted: OnExhausted::Detach,
        establishes_sessions: true,
    }])
    .ok()
}

fn serves(raised: &file_system::Client<PortTransport>) -> u64 {
    let Some(path) = Str::new(CREATED_FILE) else {
        return REFUSED_EXIT_CODE;
    };
    match raised.open(path, AccessMode::ReadWrite, OpenFlags::CREATE) {
        Ok(Ok(opened)) if opened.kind == NodeKind::File => OK_EXIT_CODE,
        Ok(Ok(_)) => REFUSED_EXIT_CODE,
        Ok(Err(error)) => {
            log_fmt(format_args!("raised component refused the file: {error:?}"));
            REFUSED_EXIT_CODE
        }
        Err(error) => {
            log_fmt(format_args!("raised component unreachable: {error:?}"));
            UNRESPONSIVE_EXIT_CODE
        }
    }
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    log_fmt(format_args!("panic: {info}"));
    thread_exit(PANIC_EXIT_CODE)
}
