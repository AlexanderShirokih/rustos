//! Транспортный контекст потока для кросс-AS рандеву.

use alloc::sync::Arc;

use kbase::MutexCell;
use memory::{memory_mapper::MemoryMapper, virtual_address::VirtualAddress};

use crate::handle_table::HandleTable;

/// Kernel-резидентный IPC-буфер, разделяемый kernel-потоком и
/// рандеву-переносом. Тот же `#[repr(C)]`-layout, что и user-буфер.
pub type KernelIpcBuffer = Arc<MutexCell<syscall::IpcBuffer>>;

#[derive(Clone)]
pub(crate) enum BufferAccess {
    User {
        mapper: Arc<dyn MemoryMapper + Send + Sync>,
        ipc_buffer_va: VirtualAddress,
    },
    Kernel {
        buffer: KernelIpcBuffer,
    },
}

/// Транспортный контекст потока для кросс-AS рандеву: доступ к телу его
/// IPC-буфера (`BufferAccess`) и его handle-таблице. Снимок берётся до
/// того, как поток уйдёт спать.
///
/// `badge` - значок port-хендла, через который отправитель инициировал send/call.
/// После успешного рандеву значок отправителя записывается в `IpcBuffer.badge` получателя.
#[derive(Clone)]
pub struct ThreadTransport {
    pub(crate) access: BufferAccess,
    pub(crate) handle_table: Arc<MutexCell<HandleTable>>,
    pub(crate) badge: u64,
}

impl ThreadTransport {
    /// Транспорт user-потока: тело в буфере, замапленном в его user-AS.
    pub fn new(
        mapper: Arc<dyn MemoryMapper + Send + Sync>,
        ipc_buffer_va: VirtualAddress,
        handle_table: Arc<MutexCell<HandleTable>>,
    ) -> Self {
        Self {
            access: BufferAccess::User {
                mapper,
                ipc_buffer_va,
            },
            handle_table,
            badge: 0,
        }
    }

    /// Транспорт kernel-потока с телом в [`KernelIpcBuffer`]. Маппинг user-AS такому потоку не
    /// требуется.
    pub fn new_kernel(buffer: KernelIpcBuffer, handle_table: Arc<MutexCell<HandleTable>>) -> Self {
        Self {
            access: BufferAccess::Kernel { buffer },
            handle_table,
            badge: 0,
        }
    }

    /// Ставит значок port-хендла, через который идёт операция. При переносе сообщения значок
    /// записывается в `IpcBuffer.badge` получателя. `0` означает "без значка".
    pub fn with_badge(mut self, badge: u64) -> Self {
        self.badge = badge;
        self
    }
}
