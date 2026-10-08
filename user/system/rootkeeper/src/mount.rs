use alloc::{string::String, vec::Vec};

use component_abi::{Attributes, RequirementDeclaration};
use component_startup::session;
use filesystem_abi::{MountEnded, PATH_MAX, file_system, file_system_mount};
use ipc::{
    CallError, Protocol as _,
    wire::{IpcError, Str},
};
use kernel_objects::{Port, Session};
use runtime::{JoinHandle, Priority, Timeout, spawn_owned};
use syscall::{Handle, kind};
use userland_image::BootPlan;

use crate::{BootFailure, RootSystem};

const MOUNT_STACK_SIZE: u64 = 0x8000;

const MOUNT_JOIN_TIMEOUT: Timeout = Timeout::from_secs(5);

#[derive(Default)]
pub(crate) struct MountThreads(Vec<JoinHandle>);

impl Drop for MountThreads {
    fn drop(&mut self) {
        // Поток монтирования завершается, когда закрыты порты остальных полей системы, и
        // `RootSystem` объявляет это поле последним. Без `join` стек потока остаётся выданным.
        for thread in self.0.drain(..) {
            let _ = thread.join(MOUNT_JOIN_TIMEOUT);
        }
    }
}

pub(crate) fn start(
    system: &mut RootSystem<'_>,
    plan: BootPlan<'_>,
    kernel: Handle<kind::Port>,
) -> Result<(), BootFailure> {
    for component in plan.components().skip(1) {
        let Some(path) = component.mountpoint() else {
            continue;
        };
        let path = filesystem_abi::Path::new(path).map_err(|_| BootFailure::BadImage)?;
        let importer = system
            .importer_for(
                &[
                    crate::ImporterRequirement {
                        declaration: RequirementDeclaration::session(
                            "filesystem",
                            file_system::Protocol::ID,
                        ),
                    },
                    crate::ImporterRequirement {
                        declaration: RequirementDeclaration::session(
                            "mount",
                            file_system_mount::Protocol::ID,
                        ),
                    },
                ],
                "filesystem",
                component.name(),
            )
            .map_err(|_| BootFailure::BadImage)?;
        let path = String::from(path.as_str());
        let thread = spawn_owned(
            move || mount(&importer, &path, kernel),
            MOUNT_STACK_SIZE,
            Priority::new(1),
        )
        .map_err(|_| BootFailure::Budget(crate::BudgetFailure::StartupPort))?;
        system.mounts.0.push(thread);
    }
    Ok(())
}

fn mount(importer: &Port, path: &str, kernel: Handle<kind::Port>) -> u32 {
    let parent = importer.as_raw();
    loop {
        let Some(filesystem) = requested::<file_system::Protocol>(parent, "filesystem", kernel)
        else {
            return 1;
        };

        let Some(mount) = requested::<file_system_mount::Protocol>(parent, "mount", kernel) else {
            return 1;
        };
        let Some(path) = Str::<PATH_MAX>::new(path) else {
            crate::system::log_line(kernel, format_args!("mount path too long"));
            return 1;
        };
        match mount
            .client()
            .with_wait(Timeout::INFINITE)
            .mount(path, filesystem)
        {
            Err(CallError::Ipc(IpcError::PeerClosed)) | Ok(MountEnded::FileSystemGone) => {}
            Ok(MountEnded::Closed | MountEnded::Replaced) => return 0,
            failure => {
                crate::system::log_line(
                    kernel,
                    format_args!("mount {} failed: {failure:?}", path.as_str()),
                );
                return 1;
            }
        }
    }
}

fn requested<P: ipc::Protocol>(
    parent: Handle<kind::Port>,
    requirement: &str,
    kernel: Handle<kind::Port>,
) -> Option<Session<P>> {
    match session::<P>(parent, requirement, Attributes::empty(), Timeout::INFINITE) {
        Ok(session) => Some(session),
        failure => {
            crate::system::log_line(
                kernel,
                format_args!("mount requirement {requirement} failed: {failure:?}"),
            );
            None
        }
    }
}
