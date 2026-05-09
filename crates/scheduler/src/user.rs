use alloc::{sync::Arc, vec::Vec};
use core::ptr::NonNull;

use kobject::{Handle, HandleId};
use memory::{user_vm_allocator::UserVmAllocator, virtual_address::VirtualAddress};

use crate::{AddressSpace, Priority, ProcessId, SpawnError, ThreadId};

/// Значение, передаваемое первому user-thread'у через ABI-аргумент.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct UserBootstrapArg(pub u64);

impl UserBootstrapArg {
    pub const ZERO: Self = Self(0);
}

/// Параметры первого входа в user-thread.
pub struct UserEntry {
    pub kernel_stack_top: NonNull<u8>,
    pub user_pc: VirtualAddress,
    pub user_sp: VirtualAddress,
    pub arg: UserBootstrapArg,
}

/// Параметры initial handle'ов для нового user-процесса.
pub struct UserProcessLaunch {
    pub bootstrap_arg: UserBootstrapArg,
    pub initial_handles: Vec<Handle>,
    pub bootstrap_handle_index: Option<usize>,
}

impl UserProcessLaunch {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn bootstrap_arg(mut self, arg: UserBootstrapArg) -> Self {
        self.bootstrap_arg = arg;
        self
    }

    pub fn initial_handles(mut self, handles: Vec<Handle>) -> Self {
        self.initial_handles = handles;
        self
    }

    pub fn bootstrap_handle(mut self, index: usize) -> Self {
        self.bootstrap_handle_index = Some(index);
        self
    }
}

impl Default for UserProcessLaunch {
    fn default() -> Self {
        Self {
            bootstrap_arg: UserBootstrapArg::ZERO,
            initial_handles: Vec::new(),
            bootstrap_handle_index: None,
        }
    }
}

/// Полностью подготовленное описание user-процесса для scheduler-а.
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

#[derive(Debug)]
pub struct UserProcessLaunchInfo {
    pub process_id: ProcessId,
    pub thread_id: ThreadId,
    pub initial_handle_ids: Vec<HandleId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreparedUserProcessError {
    Spawn(SpawnError),
    InvalidBootstrapHandle,
    TooManyInitialHandles,
}
