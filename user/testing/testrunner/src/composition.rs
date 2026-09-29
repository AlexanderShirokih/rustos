//! Обвязка тестов с планом: `RootSystem` плана под замком, поток оборота, импортёры и
//! монтирования тестов.

use alloc::{collections::BTreeMap, format, string::String, sync::Arc, vec::Vec};
use core::{
    num::NonZeroU64,
    ops::ControlFlow,
    sync::atomic::{AtomicBool, Ordering},
};

use component_abi::{RequirementDeclaration, SessionError, component_startup as startup_contract};
use filesystem_abi::{FileSystemError, file_system, file_system_mount, namespace};
use ipc::{
    Protocol,
    wire::{IpcError, Str},
};
use kernel_objects::{Error, Port, Resource, Session, Signal};
use log_abi::log;
use rootkeeper::{BUDGET_RETURN_TIMEOUT, RootSystem, SystemImage, turn};
use runtime::{JoinHandle, Mutex, PortTransport, Priority, Timeout, spawn_owned};
use spin::Once;
use syscall::{Handle, SIGNALED, SyscallError, kind};
use userland_image::{BootPlan, ComponentSpec, decode_plan, encode_plan};

pub(crate) const BOOT_PLAN_ENTRY: &str = "/testrunner/plan";

pub(crate) const ROUTER_NAME: &str = "namespace-router";

const IMPORTER_WAIT: Timeout = Timeout::from_secs(5);

const TURN: Timeout = Timeout::from_millis(20);
const TURN_STACK_SIZE: u64 = 0x10000;
const JOIN_TIMEOUT: Timeout = Timeout::from_secs(5);
const BUDGET_POLL: Timeout = Timeout::from_millis(10);
const MOUNT_STACK_SIZE: u64 = 0x10000;

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
        logs: impl Fn(NonZeroU64) -> Option<Session<log::Protocol>>,
    ) -> Self {
        await_budget(&plan);
        let system = RootSystem::bring_up(image(), plan, crate::bootstrap_handle(), logs)
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

    pub(crate) fn add(
        &self,
        component: rootkeeper::Component<'static>,
        log: Session<log::Protocol>,
    ) {
        self.try_add(component, log)
            .expect("runtime component added");
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
            spawn_owned(
                move || {
                    while turning.load(Ordering::Acquire) && turn().is_continue() {}
                    0
                },
                TURN_STACK_SIZE,
                Priority::new(1),
            )
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
        self.session_at(requirement, 0)
    }

    pub(crate) fn session_at<P: Protocol>(&self, requirement: &str, index: u16) -> Session<P> {
        let session = request_at::<P>(self.handle(), requirement, index, IMPORTER_WAIT)
            .expect("session call")
            .expect("session granted");
        if P::ID == namespace::Protocol::ID
            && let Some(root) = self.namespace_roots.get(requirement)
        {
            let client = namespace::Client::new(PortTransport::new(session.handle()));
            let scoped = client
                .open_root(Str::new(root).expect("view root fits"))
                .expect("open_root call")
                .expect("view opened");
            return Session::from_handle(Port::from(scoped.into_handle()));
        }
        session
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
        let thread = spawn_owned(
            move || {
                let ended = file_system_mount::Client::new(PortTransport::new(raw))
                    .with_wait(Timeout::INFINITE)
                    .mount(
                        Str::new(&mounted_path).expect("mount path fits"),
                        filesystem,
                    );
                if matches!(ended, Ok(Ok(_))) {
                    MOUNT_ENDED
                } else {
                    MOUNT_FAILED
                }
            },
            MOUNT_STACK_SIZE,
            Priority::new(1),
        )
        .expect("mount thread starts");
        let _ = namespace::Client::new(PortTransport::new(&view))
            .with_wait(IMPORTER_WAIT)
            .resolve(Str::new("/").expect("root path fits"))
            .expect("the view of the prefix answers");
        Self {
            admin: Some(admin),
            thread: Some(thread),
            path,
        }
    }

    pub(crate) fn mount(
        &self,
        filesystem: Session<file_system::Protocol>,
    ) -> Result<Result<(), FileSystemError>, IpcError> {
        let admin = self.admin.as_ref().expect("admin session is held");
        file_system_mount::Client::new(PortTransport::new(admin))
            .with_wait(IMPORTER_WAIT)
            .mount(Str::new(&self.path).expect("mount path fits"), filesystem)
            .map(|ended| ended.map(drop))
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
) -> Result<Result<Session<P>, SessionError>, IpcError> {
    request_at(importer, requirement, 0, wait)
}

pub(crate) fn request_at<P: Protocol>(
    importer: Handle<kind::Port>,
    requirement: &str,
    index: u16,
    wait: Timeout,
) -> Result<Result<Session<P>, SessionError>, IpcError> {
    let parent = startup_contract::Client::new(PortTransport::new(importer)).with_wait(wait);
    component_startup::session::<P>(&parent, requirement, index)
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
