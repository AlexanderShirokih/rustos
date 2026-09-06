use ipc::{MessageLen, Transport, wire::IpcError};
use kernel_objects::{Error, Port, Process, Resource};
use runtime::{PortTransport, Timeout};
use userland::EntryView;
use userland_loader::launch_entry;

const BUDGET_RETURN_TIMEOUT: Timeout = Timeout::from_secs(5);
const OWNER_DROP_EXIT_CODE: u32 = 1;

/// Наблюдаемое состояние managed-компонента.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedStatus {
    /// Startup-транзакция ещё не завершена.
    Launching,
    /// Startup-кадр доставлен, а компонент владеет запущенным экземпляром.
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
    /// Слотов компонента больше, чем содержит стартовая транзакция.
    TooManySlots,
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
///
/// Состояние - [`ManagedStatus`]: [`Self::new`] создаёт его в `Launching`, дальше
/// переходами ведают [`Self::launch`] и [`Self::observe_exit`]. Уход значения из
/// области видимости завершает работающий процесс.
pub struct ManagedComponent<R> {
    state: ManagedState<R>,
    reserve: Resource,
    budget_pages: u64,
    generation: u32,
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
    type Resources = <PortTransport as Transport>::Resources;

    fn take_resources(&self) -> Self::Resources {
        self.inner.take_resources()
    }

    fn write_message(&self, bytes: &[u8], handles: &[u32]) -> Result<(), IpcError> {
        self.inner.write_message(bytes, handles)
    }

    fn read_message(&self, bytes: &mut [u8], handles: &mut [u32]) -> Result<MessageLen, IpcError> {
        self.inner.read_message(bytes, handles)
    }

    fn wait_readable(&self, timeout: Timeout) -> Result<(), IpcError> {
        self.inner.wait_readable(timeout)
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
}

impl<R> ManagedComponent<R> {
    /// Резервирует `budget_pages` страниц под компонент и создаёт его в
    /// состоянии [`ManagedStatus::Launching`].
    ///
    /// Резерв принадлежит компоненту до его уничтожения. Каждый запуск нарезает
    /// ресурс экземпляра из него. Резерв - капа-
    /// потомок `parent_resource`, и закрытие родительской капы его отзывает.
    pub fn new(parent_resource: &Resource, budget_pages: u64) -> Result<Self, ManagedStartupError> {
        Ok(Self {
            state: ManagedState::Launching,
            reserve: parent_resource.split_when_available(budget_pages, BUDGET_RETURN_TIMEOUT)?,
            budget_pages,
            generation: 0,
        })
    }

    /// Номер текущей попытки запуска. Первый запуск даёт `1`.
    #[must_use]
    pub const fn generation(&self) -> u32 {
        self.generation
    }

    /// Текущее состояние компонента. Переходы делают [`Self::launch`] и
    /// [`Self::observe_exit`].
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

    /// Выполняет startup-транзакцию на резерве компонента с typed roots `R`.
    ///
    /// Запуск начинает новую incarnation и поднимает [`generation`](Self::generation).
    /// `name` называет процесс экземпляра. Компонент в состоянии `Running` уже
    /// владеет экземпляром, и повторный запуск отвергается `InvalidState`.
    ///
    /// Успешный запуск переводит компонент в [`ManagedStatus::Running`]. Отказ
    /// переводит его в [`ManagedStatus::Failed`] с причиной [`ManagedFailure::Startup`].
    pub fn launch<'a, V: EntryView<'a>>(
        &mut self,
        entry: &V,
        name: &str,
        startup_timeout: Timeout,
        create_roots: impl FnOnce() -> Result<R, ManagedStartupError>,
        send_startup: impl FnOnce(BoundedStartupTransport, &mut R) -> Result<(), ManagedStartupError>,
    ) -> Result<(), ManagedStartupError> {
        if matches!(self.state, ManagedState::Running(_)) {
            return Err(ManagedStartupError::InvalidState);
        }
        if startup_timeout == Timeout::INFINITE {
            return self.fail_startup(ManagedStartupError::InfiniteTimeout);
        }
        self.generation += 1;

        // Ресурс экземпляра ограничен этим блоком. Неудачный запуск освобождает его
        // концом блока, до подтверждения возврата бюджета.
        let launch = {
            let child_resource = match self.reserve.split(self.budget_pages) {
                Ok(resource) => resource,
                Err(error) => return self.fail_startup(error.into()),
            };
            let mut roots = match create_roots() {
                Ok(roots) => roots,
                Err(error) => return self.fail_startup(error),
            };
            launch_entry(entry, Some(name), &child_resource, |startup| {
                let transport = bounded_transport(startup, startup_timeout);
                send_startup(transport, &mut roots)
            })
            .map(|process| RunningComponent {
                roots,
                process,
                resource: child_resource,
            })
        };

        match launch {
            Ok(running) => {
                self.state = ManagedState::Running(running);
                Ok(())
            }
            Err(error) => {
                if let Err(cleanup) = self.confirm_budget_return() {
                    return self.fail_startup(cleanup.into());
                }
                self.fail_startup(error)
            }
        }
    }

    /// Завершает живой экземпляр и ждёт его с конечным сроком.
    ///
    /// Итоговое состояние подчиняется правилу [`Self::observe_exit`].
    pub fn stop(&mut self, exit_code: u32, timeout: Timeout) -> Result<(), ManagedStartupError> {
        let running = self.running().ok_or(ManagedStartupError::InvalidState)?;
        running
            .process
            .terminate(exit_code)
            .map_err(ManagedStartupError::Runtime)?;
        self.observe_exit(timeout)
    }

    /// Ждёт завершения `Running`-процесса и сохраняет его exit code.
    ///
    /// Код `0` переводит компонент в [`ManagedStatus::Exited`], любой другой - в
    /// [`ManagedStatus::Failed`] с причиной [`ManagedFailure::Exit`].
    pub fn observe_exit(&mut self, timeout: Timeout) -> Result<(), ManagedStartupError> {
        let exit_code = match &self.state {
            ManagedState::Running(running) => {
                running.process.join(timeout)?;
                running.process.exit_code()?
            }
            _ => return Err(ManagedStartupError::InvalidState),
        };

        let ManagedState::Running(running) =
            core::mem::replace(&mut self.state, ManagedState::Launching)
        else {
            unreachable!("state was checked before replacement");
        };
        let RunningComponent {
            roots,
            process,
            resource,
        } = running;
        drop(roots);
        drop(process);
        drop(resource);
        let cleanup = self.confirm_budget_return();

        self.state = if exit_code == 0 {
            ManagedState::Exited
        } else {
            ManagedState::Failed(ManagedFailure::Exit(exit_code))
        };
        cleanup.map_err(ManagedStartupError::Runtime)
    }

    fn confirm_budget_return(&self) -> Result<(), Error> {
        // До уничтожения последнего объекта ушедшего поддерева страницы резерву
        // не принадлежат. Возврат подтверждается повторным выделением лимита.
        self.reserve
            .split_when_available(self.budget_pages, BUDGET_RETURN_TIMEOUT)
            .map(drop)
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
        inner: PortTransport::client(startup.handle()).with_send_timeout(timeout),
    }
}
