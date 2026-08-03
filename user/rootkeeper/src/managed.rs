use ipc::{MessageLen, Transport, wire::IpcError};
use runtime::{Error, Port, PortTransport, Process, Resource, Signal, Timeout};
use syscall::{SIGNALED, SyscallError};
use userland::EntryView;
use userland_loader::launch_entry;

const BUDGET_RETURN_ATTEMPTS: usize = 1_000;
const BUDGET_RETURN_WAIT_NS: u64 = 5_000_000;
const OWNER_DROP_EXIT_CODE: u32 = 1;

/// Стабильный идентификатор managed-компонента.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComponentId(u32);

/// Наблюдаемое состояние managed-компонента.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedStatus {
    /// Startup-транзакция ещё не завершена.
    Launching,
    /// Startup-кадр доставлен, а launcher владеет экземпляром запуска.
    Running,
    /// Процесс штатно завершился с кодом `0`.
    Exited,
    /// Startup либо запущенный процесс завершились с ошибкой.
    Failed,
}

/// Ошибка startup-транзакции.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedStartupError {
    /// Отказ userspace runtime либо loader-а.
    Runtime(Error),
    /// Отказ кодирования или доставки typed startup-кадра.
    Delivery(IpcError),
    /// Операция не разрешена в текущем состоянии компонента.
    InvalidState,
    /// Managed startup требует конечного send timeout.
    InfiniteTimeout,
}

/// Причина состояния [`ManagedStatus::Failed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedFailure {
    /// Startup-транзакция не была завершена.
    Startup(ManagedStartupError),
    /// Запущенный процесс завершился с ненулевым кодом.
    Exit(u32),
}

/// Владение одним успешно запущенным экземпляром.
#[derive(Debug)]
pub struct RunningComponent<R> {
    roots: R,
    process: Process,
    resource: Resource,
    _reserved_budget: Resource,
}

/// Единственный transport одноразового startup-кадра. Его конечный send
/// timeout задаёт [`ManagedComponent`], а вызывающий не может его заменить.
pub struct BoundedStartupTransport {
    inner: PortTransport,
}

enum ManagedState<R> {
    Launching,
    Running(RunningComponent<R>),
    Exited,
    Failed(ManagedFailure),
}

/// Managed-компонент с capability roots конкретного типа `R`.
pub struct ManagedComponent<R> {
    id: ComponentId,
    state: ManagedState<R>,
}

impl ComponentId {
    /// Создаёт идентификатор из устойчивого номера deployment-модели.
    #[must_use]
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// Возвращает устойчивый номер компонента.
    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }
}

impl From<Error> for ManagedStartupError {
    fn from(error: Error) -> Self {
        Self::Runtime(error)
    }
}

impl From<IpcError> for ManagedStartupError {
    fn from(error: IpcError) -> Self {
        Self::Delivery(error)
    }
}

impl Transport for BoundedStartupTransport {
    fn write_message(&self, bytes: &[u8], handles: &[u32]) -> Result<(), IpcError> {
        self.inner.write_message(bytes, handles)
    }

    fn read_message(&self, bytes: &mut [u8], handles: &mut [u32]) -> Result<MessageLen, IpcError> {
        self.inner.read_message(bytes, handles)
    }

    fn wait_readable(&self, timeout_ns: u64) -> Result<(), IpcError> {
        self.inner.wait_readable(timeout_ns)
    }
}

impl<R> RunningComponent<R> {
    /// Capability roots текущего экземпляра.
    #[must_use]
    pub const fn roots(&self) -> &R {
        &self.roots
    }

    /// Waitable process текущего экземпляра.
    #[must_use]
    pub const fn process(&self) -> &Process {
        &self.process
    }

    /// Дочерний metering resource текущего экземпляра.
    #[must_use]
    pub const fn resource(&self) -> &Resource {
        &self.resource
    }
}

impl<R> ManagedComponent<R> {
    /// Создаёт компонент в состоянии [`ManagedStatus::Launching`].
    #[must_use]
    pub const fn new(id: ComponentId) -> Self {
        Self {
            id,
            state: ManagedState::Launching,
        }
    }

    /// Стабильный идентификатор компонента.
    #[must_use]
    pub const fn id(&self) -> ComponentId {
        self.id
    }

    /// Текущее состояние компонента.
    #[must_use]
    pub const fn status(&self) -> ManagedStatus {
        match self.state {
            ManagedState::Launching => ManagedStatus::Launching,
            ManagedState::Running(_) => ManagedStatus::Running,
            ManagedState::Exited => ManagedStatus::Exited,
            ManagedState::Failed(_) => ManagedStatus::Failed,
        }
    }

    /// Владение текущим экземпляром в состоянии `Running`.
    #[must_use]
    pub const fn running(&self) -> Option<&RunningComponent<R>> {
        match &self.state {
            ManagedState::Running(running) => Some(running),
            _ => None,
        }
    }

    /// Сохранённая причина отказа компонента.
    #[must_use]
    pub const fn failure(&self) -> Option<ManagedFailure> {
        match self.state {
            ManagedState::Failed(failure) => Some(failure),
            _ => None,
        }
    }

    /// Выполняет startup-транзакцию с отдельным budget и typed roots `R`.
    pub fn launch<'a, V: EntryView<'a>>(
        &mut self,
        entry: &V,
        parent_resource: &Resource,
        budget_pages: u64,
        startup_timeout: Timeout,
        create_roots: impl FnOnce() -> Result<R, ManagedStartupError>,
        send_startup: impl FnOnce(BoundedStartupTransport, &R) -> Result<(), ManagedStartupError>,
    ) -> Result<(), ManagedStartupError> {
        if !matches!(self.state, ManagedState::Launching) {
            return Err(ManagedStartupError::InvalidState);
        }
        if startup_timeout == Timeout::INFINITE {
            return self.fail_startup(ManagedStartupError::InfiniteTimeout);
        }

        let reserved_budget = match parent_resource.split(budget_pages) {
            Ok(resource) => resource,
            Err(error) => return self.fail_startup(error.into()),
        };
        let child_resource = match reserved_budget.split(budget_pages) {
            Ok(resource) => resource,
            Err(error) => return self.fail_startup(error.into()),
        };
        let mut roots = match create_roots() {
            Ok(roots) => Some(roots),
            Err(error) => return self.fail_startup(error),
        };

        let launch = {
            let result = launch_entry(entry, &child_resource, |startup| {
                let component_roots = roots
                    .as_ref()
                    .expect("startup roots are present before delivery");
                let transport = bounded_transport(startup, startup_timeout);
                let result = send_startup(transport, component_roots);
                if result.is_err() {
                    roots = None;
                }
                result
            });

            match result {
                Ok(process) => Ok(RunningComponent {
                    roots: roots
                        .take()
                        .expect("successful startup retains capability roots"),
                    process,
                    resource: child_resource,
                    _reserved_budget: reserved_budget,
                }),
                Err(error) => Err((error, reserved_budget, child_resource)),
            }
        };

        match launch {
            Ok(running) => {
                self.state = ManagedState::Running(running);
                Ok(())
            }
            Err((error, reserved_budget, child_resource)) => {
                let reserved_budget = {
                    let _child_resource = child_resource;
                    reserved_budget
                };
                if let Err(cleanup) = wait_for_budget_return(&reserved_budget, budget_pages) {
                    return self.fail_startup(cleanup.into());
                }
                self.fail_startup(error)
            }
        }
    }

    /// Ждёт завершения `Running`-процесса и сохраняет его exit code.
    pub fn observe_exit(&mut self, timeout: Timeout) -> Result<(), ManagedStartupError> {
        let exit_code = match &self.state {
            ManagedState::Running(running) => {
                running.process.join(timeout)?;
                running.process.exit_code()?
            }
            _ => return Err(ManagedStartupError::InvalidState),
        };

        self.state = if exit_code == 0 {
            ManagedState::Exited
        } else {
            ManagedState::Failed(ManagedFailure::Exit(exit_code))
        };
        Ok(())
    }

    fn fail_startup(&mut self, error: ManagedStartupError) -> Result<(), ManagedStartupError> {
        self.state = ManagedState::Failed(ManagedFailure::Startup(error));
        Err(error)
    }
}

impl<R> Drop for ManagedComponent<R> {
    fn drop(&mut self) {
        if let ManagedState::Running(running) = &self.state {
            let _ = running.process.terminate(OWNER_DROP_EXIT_CODE);
        }
    }
}

fn bounded_transport(startup: &Port, timeout: Timeout) -> BoundedStartupTransport {
    debug_assert_ne!(timeout, Timeout::INFINITE);
    BoundedStartupTransport {
        inner: PortTransport::client(startup.handle()).with_send_timeout_ns(timeout.raw()),
    }
}

fn wait_for_budget_return(resource: &Resource, budget_pages: u64) -> Result<(), Error> {
    let timer = Signal::create()?;
    for _ in 0..BUDGET_RETURN_ATTEMPTS {
        match resource.split(budget_pages) {
            Ok(_returned_budget) => return Ok(()),
            Err(Error::Syscall(SyscallError::ResourceExhausted)) => {
                match timer.wait(SIGNALED, Timeout::from_ns(BUDGET_RETURN_WAIT_NS)) {
                    Err(Error::Syscall(SyscallError::Timeout)) => {}
                    Err(error) => return Err(error),
                    Ok(_) => unreachable!("private timer is never signaled"),
                }
            }
            Err(error) => return Err(error),
        }
    }
    Err(Error::Syscall(SyscallError::Timeout))
}
