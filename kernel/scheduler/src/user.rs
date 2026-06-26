use alloc::sync::Arc;
use core::ptr::NonNull;

use capability::{Capability, HandleId, ProcessObject, ThreadObject};
use memory::{user_vm_allocator::UserVmAllocator, virtual_address::VirtualAddress};

use crate::{AddressSpace, Priority, ProcessId, SpawnError, ThreadId};

/// Значение, передаваемое первому user-thread'у через ABI-аргумент.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct UserBootstrapArg(pub u64);

/// Параметры первого входа в user-режим.
pub struct UserEntry {
    pub kernel_stack_top: NonNull<u8>,
    pub user_pc: VirtualAddress,
    pub user_sp: VirtualAddress,
    pub arg: UserBootstrapArg,
}

/// Параметры старта user-процесса.
pub struct UserProcessLaunch {
    pub initial_handle: Capability,
}

impl UserProcessLaunch {
    pub fn new(initial_handle: Capability) -> Self {
        Self { initial_handle }
    }
}

/// Полностью подготовленное описание user-процесса для передачи в планировщик.
pub struct PreparedUserProcess {
    pub name: &'static str,
    pub priority: Priority,
    pub kernel_stack_pages: usize,
    pub address_space: Arc<AddressSpace>,
    pub user_pc: VirtualAddress,
    pub user_sp: VirtualAddress,
    pub user_vm: Option<UserVmAllocator>,
    pub launch: UserProcessLaunch,
}

pub struct UserProcessLaunchInfo {
    pub process_id: ProcessId,
    pub thread_id: ThreadId,
    pub initial_handle_id: HandleId,
    pub process_object: Arc<ProcessObject>,
    pub thread_object: Arc<ThreadObject>,
}

impl core::fmt::Debug for UserProcessLaunchInfo {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("UserProcessLaunchInfo")
            .field("process_id", &self.process_id)
            .field("thread_id", &self.thread_id)
            .field("initial_handle_id", &self.initial_handle_id)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreparedUserProcessError {
    Spawn(SpawnError),
}
