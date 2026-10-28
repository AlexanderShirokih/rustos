//! Обвязка тестов с планом: `RootSystem` плана под замком, поток оборота, импортёры и
//! монтирования тестов.

use alloc::{
    boxed::Box, collections::BTreeMap, format, rc::Rc, string::String, sync::Arc, vec::Vec,
};
use core::{
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicBool, Ordering},
};

use component_abi::{
    Attributes, RequirementDeclaration, STARTUP_REQUEST_TIMEOUT, SessionError,
    component_startup as startup_contract,
};
use component_startup::{AnyOf, Found, Providers};
use executor::Executor;
use filesystem_abi::{FileSystemError, file_system, file_system_mount, namespace};
use ipc::{CallError, Protocol, wire::IpcError};
use kernel_objects::{Error, Port, Resource, Session, Signal};
use log_abi::log;
use platform_abi::{CONTROLLER_KEY, DeviceId, Node};
use rootkeeper::{BUDGET_RETURN_TIMEOUT, RootSystem, SearchLimits, SystemImage, relay::LogSources};
use runtime::{JoinHandle, Mutex, MutexGuard, PortTransport, Timeout, spawn_owned};
use spin::Once;
use syscall::{Handle, SIGNALED, SyscallError, kind};
use userland_image::{BootPlan, ComponentSpec, ExportIndex, decode_plan, encode_plan};

use crate::stress::{answered, expire};

pub(crate) const BOOT_PLAN_ENTRY: &str = "/testrunner/plan";

pub(crate) const ROUTER_NAME: &str = "mounts";

type Serving = Pin<Box<dyn Future<Output = ()>>>;

const IMPORTER_WAIT: Timeout = Timeout::from_secs(5);

const TURN: Timeout = Timeout::from_millis(20);
const JOIN_TIMEOUT: Timeout = Timeout::from_secs(5);
const BUDGET_POLL: Timeout = Timeout::from_millis(10);

const MOUNT_ENDED: u32 = 0;
const MOUNT_FAILED: u32 = 1;
const MOUNT_CANCELLED: u32 = 2;

static IMAGE: Once<SystemImage> = Once::new();

pub(crate) struct Composition {
    _turn: TurnThread,
    system: Arc<Mutex<RootSystem<'static>>>,
}

pub(crate) struct TurnThread {
    turning: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle>>,
}

pub(crate) struct Importer {
    port: Port,
    namespace_roots: BTreeMap<String, String>,
}

pub(crate) struct Mounted {
    admin: Option<Port>,
    thread: Option<JoinHandle>,
    path: String,
}

pub(crate) struct Import {
    pub(crate) declaration: RequirementDeclaration,
    pub(crate) namespace_root: Option<&'static str>,
}

pub(crate) const fn view(name: &'static str, root: &'static str) -> Import {
    Import {
        declaration: RequirementDeclaration::all(name, namespace::Protocol::ID),
        namespace_root: Some(root),
    }
}

pub(crate) const fn admin(name: &'static str) -> Import {
    Import {
        declaration: RequirementDeclaration::all(name, file_system_mount::Protocol::ID),
        namespace_root: None,
    }
}

pub(crate) fn image() -> &'static SystemImage {
    IMAGE.call_once(|| {
        SystemImage::acquire(crate::bootstrap_handle()).expect("userland image is mapped")
    })
}

pub(crate) fn encoded(components: &[ComponentSpec<'_>]) -> BootPlan<'static> {
    let bytes = encode_plan(components).expect("scenario plan encodes");
    decode_plan(Vec::leak(bytes)).expect("scenario plan decodes")
}

pub(crate) fn pause(timeout: Timeout) {
    let timer = Signal::create().expect("pause timer");
    let _ = timer.wait(SIGNALED, timeout);
}

impl Composition {
    pub(crate) fn start(
        plan: BootPlan<'static>,
        index: ExportIndex<'static>,
        sources: LogSources,
        limits: SearchLimits,
    ) -> Self {
        await_budget(&plan);

        let system = RootSystem::bring_up(
            image(),
            plan,
            index,
            crate::bootstrap_handle(),
            sources,
            limits,
        )
        .expect("composition started");
        let system = Arc::new(Mutex::new(system));
        let turned = Arc::clone(&system);
        Self {
            _turn: TurnThread::start(move |executor| Box::pin(rootkeeper::serve(executor, turned))),
            system,
        }
    }

    pub(crate) fn importer(&self, requirements: &[Import]) -> Importer {
        let declarations: Vec<_> = requirements
            .iter()
            .map(|requirement| rootkeeper::ImporterRequirement {
                declaration: requirement.declaration,
            })
            .collect();
        let port = self
            .system
            .lock()
            .importer(&declarations)
            .expect("importer requirements are valid");
        let namespace_roots = requirements
            .iter()
            .filter_map(|requirement| {
                requirement.namespace_root.map(|root| {
                    (
                        String::from(requirement.declaration.name()),
                        String::from(if root.is_empty() { "/" } else { root }),
                    )
                })
            })
            .collect();
        Importer {
            port,
            namespace_roots,
        }
    }

    pub(crate) fn try_add(
        &self,
        component: rootkeeper::Component<'static>,
        log: Session<log::Protocol>,
    ) -> Result<u16, rootkeeper::AddFailure> {
        self.system.lock().add(component, log)
    }

    pub(crate) fn terminate(&self, name: &str, code: u32) {
        self.system.lock().terminate(name, code);
    }

    pub(crate) fn hold(&self) -> MutexGuard<'_, RootSystem<'static>> {
        self.system.lock()
    }
}

impl TurnThread {
    pub(crate) fn start(build: impl FnOnce(&Rc<Executor>) -> Serving + Send + 'static) -> Self {
        let turning = Arc::new(AtomicBool::new(true));
        let thread = {
            let turning = Arc::clone(&turning);
            spawn_owned(move || {
                let executor = Rc::new(Executor::new().expect("turn executor"));
                let served = build(&executor);
                executor.block_on(async {
                    let mut served = core::pin::pin!(served);
                    while turning.load(Ordering::Acquire) {
                        if executor::timeout(TURN, served.as_mut()).await.is_some() {
                            break;
                        }
                    }
                });
                0
            })
            .expect("turn thread starts")
        };
        Self {
            turning,
            thread: Mutex::new(Some(thread)),
        }
    }
}

impl Drop for TurnThread {
    fn drop(&mut self) {
        self.turning.store(false, Ordering::Release);
        if let Some(thread) = self.thread.lock().take() {
            thread.join(JOIN_TIMEOUT).expect("turn thread stops");
        }
    }
}

impl Importer {
    pub(crate) fn session<P: Protocol>(&self, requirement: &str) -> Session<P> {
        let session = answered(
            request::<P>(self.handle(), requirement, IMPORTER_WAIT),
            format_args!("requirement={requirement}"),
            "session",
            IMPORTER_WAIT,
            "session granted",
        );
        if P::ID == namespace::Protocol::ID
            && let Some(root) = self.namespace_roots.get(requirement)
        {
            let client = namespace::Client::new(PortTransport::new(session.handle()));
            let scoped = client.open_root(root).wait().expect("view opened");
            return Session::from_handle(Port::from(scoped.into_handle()));
        }
        session
    }

    pub(crate) fn found<P: Protocol>(&self, requirement: &str) -> Vec<Found> {
        self.found_with::<P>(requirement, Attributes::empty())
    }

    pub(crate) fn found_with<P: Protocol>(
        &self,
        requirement: &str,
        attributes: Attributes<'_>,
    ) -> Vec<Found> {
        drained(&answered(
            self.enumeration::<P>(requirement, attributes),
            format_args!("requirement={requirement}"),
            "enumeration",
            IMPORTER_WAIT,
            "enumeration granted",
        ))
    }

    pub(crate) fn enumeration<P: Protocol>(
        &self,
        requirement: &str,
        attributes: Attributes<'_>,
    ) -> Result<Session<startup_contract::Protocol>, CallError<SessionError>> {
        self.search(async |parent| {
            component_startup::all::<P>(&parent, requirement, attributes).await
        })
    }

    pub(crate) fn watch<P: Protocol>(
        &self,
        requirement: &str,
        attributes: Attributes<'_>,
    ) -> Result<Session<startup_contract::Protocol>, CallError<SessionError>> {
        self.search(async |parent| {
            component_startup::watch::<P>(&parent, requirement, attributes).await
        })
    }

    pub(crate) fn port<P: Protocol>(&self, requirement: &str) -> Port {
        self.session::<P>(requirement).into_handle()
    }

    pub(crate) fn handle(&self) -> Handle<kind::Port> {
        self.port.as_raw()
    }

    fn search(
        &self,
        request: impl AsyncFnOnce(
            component_startup::AsyncStartup,
        ) -> Result<
            Session<startup_contract::Protocol>,
            CallError<SessionError>,
        >,
    ) -> Result<Session<startup_contract::Protocol>, CallError<SessionError>> {
        let executor = crate::executor::executor();
        let parent =
            component_startup::AsyncStartup::new(executor::PortChannel::new(self.handle()))
                .with_wait(IMPORTER_WAIT);

        executor.block_on(request(parent))
    }
}

impl Mounted {
    pub(crate) fn new(
        importer: &Importer,
        requirement: &str,
        filesystem: Session<file_system::Protocol>,
    ) -> Self {
        let admin = importer.port::<file_system_mount::Protocol>(&format!("{requirement}-admin"));
        let view = importer.port::<namespace::Protocol>(requirement);
        let path = importer
            .namespace_roots
            .get(requirement)
            .expect("mount test has a view")
            .clone();
        let mounted_path = path.clone();
        let raw = admin.as_raw();
        let thread = spawn_owned(move || {
            let ended = file_system_mount::Client::new(PortTransport::new(raw))
                .with_wait(Timeout::INFINITE)
                .mount(&mounted_path, filesystem)
                .wait();
            match ended {
                Ok(_) => MOUNT_ENDED,
                Err(CallError::Ipc(IpcError::PeerClosed)) => MOUNT_CANCELLED,
                Err(_) => MOUNT_FAILED,
            }
        })
        .expect("mount thread starts");
        let resolved = namespace::Client::new(PortTransport::new(&view))
            .with_wait(IMPORTER_WAIT)
            .resolve("/")
            .wait();
        match resolved {
            Err(CallError::Ipc(IpcError::Timeout)) => expire(
                format_args!("requirement={requirement}"),
                "resolve",
                "call",
                IMPORTER_WAIT,
            ),
            Err(CallError::Ipc(error)) => panic!("the view of the prefix answers: {error:?}"),
            Ok(_) | Err(CallError::Remote(_)) => {}
        }
        Self {
            admin: Some(admin),
            thread: Some(thread),
            path,
        }
    }

    pub(crate) fn mount(
        &self,
        filesystem: Session<file_system::Protocol>,
    ) -> Result<(), CallError<FileSystemError>> {
        let admin = self.admin.as_ref().expect("admin session is held");
        file_system_mount::Client::new(PortTransport::new(admin))
            .with_wait(IMPORTER_WAIT)
            .mount(&self.path, filesystem)
            .wait()
            .map(drop)
    }

    pub(crate) fn ended(&self, wait: Timeout) -> bool {
        let thread = self.thread.as_ref().expect("mount thread is held");
        thread.wait(wait).is_ok() && thread.exit_code() == Ok(MOUNT_ENDED)
    }

    pub(crate) fn cancelled(&self, wait: Timeout) -> bool {
        let thread = self.thread.as_ref().expect("mount thread is held");
        thread.wait(wait).is_ok() && thread.exit_code() == Ok(MOUNT_CANCELLED)
    }

    pub(crate) fn close_admin(&mut self) {
        drop(self.admin.take());
    }
}

impl Drop for Mounted {
    fn drop(&mut self) {
        drop(self.admin.take());
        if let Some(thread) = self.thread.take() {
            thread.join(JOIN_TIMEOUT).expect("mount call returns");
        }
    }
}

pub(crate) fn request<P: Protocol>(
    importer: Handle<kind::Port>,
    requirement: &str,
    wait: Timeout,
) -> Result<Session<P>, CallError<SessionError>> {
    component_startup::session::<P>(importer, requirement, Attributes::empty(), wait)
}

pub(crate) fn controller_provider<P: Protocol>(
    importer: Handle<kind::Port>,
    requirement: &'static str,
    compatibles: &[String],
) -> Session<P> {
    let executor = crate::executor::executor();
    let providers = Providers::<P>::new(importer, requirement);

    executor.block_on(async {
        let mut found = providers
            .all(AnyOf::new(
                CONTROLLER_KEY,
                compatibles.iter().map(String::as_str),
            ))
            .await
            .expect("provider enumeration");

        let found = found
            .next()
            .await
            .expect("provider export")
            .expect("provider export found");
        providers.open(&found).await.expect("provider session")
    })
}

pub(crate) fn compatibles<'a>(controllers: impl IntoIterator<Item = Node<'a>>) -> Vec<String> {
    let mut compatibles = Vec::new();
    for controller in controllers {
        if let Ok(Some(names)) = controller.strings("compatible") {
            compatibles.extend(names.map(String::from));
        }
    }
    compatibles
}

pub(crate) fn device(path: &str) -> DeviceId {
    DeviceId::new(path).expect("absolute path")
}

pub(crate) fn drained(enumeration: &Session<startup_contract::Protocol>) -> Vec<Found> {
    let mut found = Vec::new();
    while let Some(result) = {
        let executor = crate::executor::executor();
        executor.block_on(component_startup::next(
            enumeration,
            STARTUP_REQUEST_TIMEOUT,
        ))
    }
    .expect("next answered")
    {
        found.push(result);
    }
    found
}

fn await_budget(plan: &BootPlan<'_>) {
    let resource = Resource::self_resource().expect("self metering resource");
    let mut waited = Timeout::POLL;
    loop {
        let probes: Result<Vec<Resource>, Error> = plan
            .components()
            .skip(1)
            .map(|component| resource.split(component.memory_limit()))
            .collect();
        match probes {
            Ok(_) => return,
            Err(Error::Syscall(SyscallError::ResourceExhausted))
                if waited < BUDGET_RETURN_TIMEOUT =>
            {
                pause(BUDGET_POLL);
                waited = waited.saturating_add(BUDGET_POLL);
            }
            Err(Error::Syscall(SyscallError::ResourceExhausted)) => expire(
                format_args!("components={}", plan.components().count()),
                "composition_budget",
                "split",
                BUDGET_RETURN_TIMEOUT,
            ),
            Err(error) => panic!("composition budget is not available: {error:?}"),
        }
    }
}
