//! Отдельный broker сессий файловых систем компонентов плана.

use alloc::{string::String, sync::Arc, vec::Vec};

use filesystem_abi::{FileSystemError, PATH_MAX, file_system_admin};
use ipc::{
    Transport as _,
    wire::{Cap, IpcError, Str},
};
use rootkeeper_abi::rootkeeper;
use runtime::{
    BorrowedHandle, Error, JoinHandle, OwnedHandle, Port, PortTransport, Priority, Timeout,
    spawn_owned,
};
use spin::Mutex;
use syscall::{Rights, SyscallError};

const BROKER_STACK: u64 = 0x4000;
const BROKER_SHUTDOWN_TIMEOUT: Timeout = Timeout::from_secs(2);

struct Provider {
    index: u16,
    executable: String,
    control: OwnedHandle,
    timeout: Timeout,
}

#[derive(Default)]
struct Registry {
    providers: Vec<Provider>,
}

struct Serving {
    registry: Arc<Mutex<Registry>>,
    vended: Option<OwnedHandle>,
}

pub(crate) struct SessionBroker {
    root: Option<OwnedHandle>,
    thread: Option<JoinHandle>,
    registry: Arc<Mutex<Registry>>,
}

impl SessionBroker {
    pub(crate) fn start() -> runtime::Result<Self> {
        let root = Port::create()?.into_handle();
        let server = root.duplicate(Rights::READ, None)?;
        let registry = Arc::new(Mutex::new(Registry::default()));
        let serving = Serving {
            registry: Arc::clone(&registry),
            vended: None,
        };
        let thread = spawn_owned(
            move || {
                serve(server, serving);
                0
            },
            BROKER_STACK,
            Priority::new(1),
        )?;
        Ok(Self {
            root: Some(root),
            thread: Some(thread),
            registry,
        })
    }

    pub(crate) fn session(&self) -> Option<OwnedHandle> {
        self.root
            .as_ref()?
            .duplicate(Rights::WRITE | Rights::TRANSFER, None)
            .ok()
    }

    pub(crate) fn publish(
        &self,
        index: u16,
        executable: &str,
        control: BorrowedHandle<'_>,
        timeout: Timeout,
    ) -> runtime::Result<()> {
        // Корень control-порта принадлежит incarnation: отпустивший его компонент отзывает
        // вместе с ним лиз supervisor, и `bind` по такому лизу закрыт. Место в реестре
        // держит живой control-порт, а запуск компонента остаётся состоявшимся.
        let control = match control.duplicate(Rights::WRITE | Rights::TRANSFER, None) {
            Ok(control) => control,
            Err(Error::Syscall(SyscallError::Revoked)) => {
                self.withdraw(index);
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let mut registry = self.registry.lock();
        if let Some(provider) = registry
            .providers
            .iter_mut()
            .find(|provider| provider.index == index)
        {
            provider.executable = String::from(executable);
            provider.control = control;
            provider.timeout = timeout;
        } else {
            registry.providers.push(Provider {
                index,
                executable: String::from(executable),
                control,
                timeout,
            });
        }
        Ok(())
    }

    pub(crate) fn withdraw(&self, index: u16) {
        self.registry
            .lock()
            .providers
            .retain(|provider| provider.index != index);
    }

    pub(crate) fn shutdown(&mut self) {
        drop(self.root.take());
        let Some(thread) = self.thread.take() else {
            return;
        };
        if thread.thread().join(BROKER_SHUTDOWN_TIMEOUT).is_err() {
            let _ = thread.thread().terminate(0);
        }
        let _ = thread.join(BROKER_SHUTDOWN_TIMEOUT);
    }
}

impl Drop for SessionBroker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl Serving {
    fn end_frame(&mut self, delivered: bool) {
        let Some(handle) = self.vended.take() else {
            return;
        };
        if delivered {
            let _ = handle.into_raw();
        } else {
            let _ = handle.close();
        }
    }

    fn vend(&mut self, handle: OwnedHandle) -> Cap {
        let id = handle.as_raw().as_nonzero();
        self.vended = Some(handle);
        Cap::from_raw(id)
    }
}

impl rootkeeper::Service for Serving {
    fn file_system(&mut self, program: Str<'_, PATH_MAX>) -> Result<Cap, FileSystemError> {
        let registry = self.registry.lock();
        let mut matches = registry
            .providers
            .iter()
            .filter(|provider| provider.executable == program.as_str());
        let provider = matches.next().ok_or(FileSystemError::NotFound)?;
        if matches.next().is_some() {
            return Err(FileSystemError::NotSupported);
        }
        let session = file_system_admin::Client::new(PortTransport::client(&provider.control))
            .with_wait(provider.timeout)
            .bind()
            .map_err(|_| FileSystemError::Unresponsive)??;
        drop(registry);
        Ok(self.vend(OwnedHandle::adopt(session)))
    }
}

fn serve(server: OwnedHandle, mut serving: Serving) {
    let transport = PortTransport::server(server.borrow());
    loop {
        match transport.wait_readable(Timeout::INFINITE) {
            Ok(()) => {
                let delivered = rootkeeper::dispatch(&mut serving, &transport).is_ok();
                serving.end_frame(delivered);
                transport.cancel_reply();
            }
            Err(IpcError::ResourceExhausted | IpcError::Timeout) => {}
            Err(_) => break,
        }
    }
    drop(server);
}
