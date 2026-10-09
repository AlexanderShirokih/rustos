//! Обвязка тестов с планом: `RootSystem` плана под замком, поток оборота, импортёры и
//! монтирования тестов.

use alloc::{collections::BTreeMap, format, string::String, sync::Arc, vec::Vec};
use core::{
    ops::ControlFlow,
    sync::atomic::{AtomicBool, Ordering},
};

use component_abi::{
    Attributes, RequirementDeclaration, STARTUP_REQUEST_TIMEOUT, SessionError,
    component_startup as startup_contract,
};
use component_startup::Found;
use filesystem_abi::{FileSystemError, file_system, file_system_mount, namespace};
use ipc::{CallError, Protocol};
use kernel_objects::{Error, Port, Resource, Session, Signal};
use log_abi::log;
use rootkeeper::{BUDGET_RETURN_TIMEOUT, RootSystem, SystemImage, relay::LogSources, turn};
use runtime::{JoinHandle, Mutex, PortTransport, Timeout, spawn_owned};
use spin::Once;
use syscall::{Handle, SIGNALED, SyscallError, kind};
use userland_image::{BootPlan, ComponentSpec, ExportIndex, decode_plan, encode_plan};

pub(crate) const BOOT_PLAN_ENTRY: &str = "/testrunner/plan";

pub(crate) const ROUTER_NAME: &str = "mounts";

const IMPORTER_WAIT: Timeout = Timeout::from_secs(5);

const TURN: Timeout = Timeout::from_millis(20);
const JOIN_TIMEOUT: Timeout = Timeout::from_secs(5);
const BUDGET_POLL: Timeout = Timeout::from_millis(10);

const MOUNT_ENDED: u32 = 0;
const MOUNT_FAILED: u32 = 1;

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
        logs: LogSources,
    ) -> Self {
        await_budget(&plan);

        let system = RootSystem::bring_up(image(), plan, index, crate::bootstrap_handle(), logs)
            .expect("composition started");
        let system = Arc::new(Mutex::new(system));
        let turned = Arc::clone(&system);
        Self {
            _turn: TurnThread::start(move || turn(&turned, TURN)),
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
}

impl TurnThread {
    pub(crate) fn start(mut turn: impl FnMut() -> ControlFlow<()> + Send + 'static) -> Self {
        let turning = Arc::new(AtomicBool::new(true));
        let thread = {
            let turning = Arc::clone(&turning);
            spawn_owned(move || {
                while turning.load(Ordering::Acquire) && turn().is_continue() {}
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
        let session =
            request::<P>(self.handle(), requirement, IMPORTER_WAIT).expect("session granted");
        if P::ID == namespace::Protocol::ID
            && let Some(root) = self.namespace_roots.get(requirement)
        {
            let client = namespace::Client::new(PortTransport::new(session.handle()));
            let scoped = client.open_root(root).expect("view opened");
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
        drained(
            &self
                .enumeration::<P>(requirement, attributes)
                .expect("enumeration granted"),
        )
    }

    pub(crate) fn enumeration<P: Protocol>(
        &self,
        requirement: &str,
        attributes: Attributes<'_>,
    ) -> Result<Session<startup_contract::Protocol>, CallError<SessionError>> {
        let parent = startup_contract::Client::new(PortTransport::new(self.handle()))
            .with_wait(IMPORTER_WAIT);
        component_startup::all::<P>(&parent, requirement, attributes)
    }

    pub(crate) fn port<P: Protocol>(&self, requirement: &str) -> Port {
        self.session::<P>(requirement).into_handle()
    }

    pub(crate) fn handle(&self) -> Handle<kind::Port> {
        self.port.as_raw()
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
                .mount(&mounted_path, filesystem);
            if ended.is_ok() {
                MOUNT_ENDED
            } else {
                MOUNT_FAILED
            }
        })
        .expect("mount thread starts");
        let resolved = namespace::Client::new(PortTransport::new(&view))
            .with_wait(IMPORTER_WAIT)
            .resolve("/");
        if let Err(CallError::Ipc(error)) = resolved {
            panic!("the view of the prefix answers: {error:?}");
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
            .map(drop)
    }

    pub(crate) fn ended(&self, wait: Timeout) -> bool {
        let thread = self.thread.as_ref().expect("mount thread is held");
        thread.wait(wait).is_ok() && thread.exit_code() == Ok(MOUNT_ENDED)
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

pub(crate) fn drained(enumeration: &Session<startup_contract::Protocol>) -> Vec<Found> {
    let mut found = Vec::new();
    while let Some(result) =
        component_startup::next(enumeration, STARTUP_REQUEST_TIMEOUT).expect("next answered")
    {
        found.push(result);
    }
    found
}

fn await_budget(plan: &BootPlan<'_>) {
    let resource = Resource::self_resource().expect("self metering resource");
    let mut waited = 0;
    loop {
        let probes: Result<Vec<Resource>, Error> = plan
            .components()
            .skip(1)
            .map(|component| resource.split(component.memory_limit()))
            .collect();
        match probes {
            Ok(_) => return,
            Err(Error::Syscall(SyscallError::ResourceExhausted))
                if waited < BUDGET_RETURN_TIMEOUT.raw() =>
            {
                pause(BUDGET_POLL);
                waited += BUDGET_POLL.raw();
            }
            Err(error) => panic!("composition budget is not available: {error:?}"),
        }
    }
}
