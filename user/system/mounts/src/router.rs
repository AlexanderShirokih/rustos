//! Дерево монтирований и ожидания разрешения путей задачами одного executor.

use alloc::{
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
    string::String,
    vec::Vec,
};
use core::{
    cell::{Cell, RefCell},
    task::{Context, Poll, Waker},
};

use component_startup::{MintedSession, SessionError, SessionPort};
use executor::{PortChannel, Reactor};
use filesystem_abi::{
    FILE_SYSTEM_OPEN_ROOT_TIMEOUT, FileSystemError, MountEnded, PATH_MAX, Path, ResolvedPair,
    common_directory, file_system, file_system_mount, namespace, rebase,
};
use ipc::{
    CallError, IntoWireValue,
    wire::{IpcError, Session},
};
use kernel_objects::{Delivery, Port, ReceivedSession, SessionTransport};
use runtime::log_fmt;

const WAITING_REQUESTS_MAX: usize = 64;
const VIEW_WAITING_REQUESTS_MAX: usize = 8;

type FileSystemClient = Rc<file_system::Client<PortChannel<SessionTransport>>>;
type Route<'m> = Option<(&'m Mount, String, u32)>;
type ReadyRoute = (u64, FileSystemClient, String, u32);

pub(crate) struct Router {
    views: Rc<SessionPort>,
    mounts: Rc<SessionPort>,
    tree: RefCell<Tree>,
    reactor: Reactor,
}

#[derive(Default)]
struct Tree {
    views: BTreeMap<u64, String>,
    mount_sessions: BTreeSet<u64>,
    mounts: BTreeMap<String, Mount>,
    waiting: Vec<Rc<Waiting>>,
    next_mount: u64,
}

struct Mount {
    id: u64,
    badge: u64,
    filesystem: FileSystemClient,
    ending: Rc<MountEnding>,
}

#[derive(Default)]
struct MountEnding {
    reason: Cell<Option<MountEnded>>,
    waker: RefCell<Option<Waker>>,
}

struct Waiting {
    view: u64,
    request: Request,
    refused: Cell<Option<FileSystemError>>,
    waker: RefCell<Option<Waker>>,
}

struct Registration<'a> {
    tree: &'a RefCell<Tree>,
    waiting: Rc<Waiting>,
}

enum Request {
    Resolve { path: String },
    Pair { from: String, to: String },
}

impl Router {
    pub(crate) fn new(views: Rc<SessionPort>, mounts: Rc<SessionPort>, reactor: Reactor) -> Self {
        Self {
            views,
            mounts,
            tree: RefCell::new(Tree::default()),
            reactor,
        }
    }

    pub(crate) fn open_view(&self) -> Result<Port, SessionError> {
        self.open("/").map_err(|_| SessionError::NoSpace)
    }

    pub(crate) fn open_mount(&self) -> Result<Port, SessionError> {
        let MintedSession { badge, port } = self
            .mounts
            .create_session()
            .map_err(|_| SessionError::NoSpace)?;
        self.tree.borrow_mut().mount_sessions.insert(badge.get());
        Ok(port)
    }

    pub(crate) fn view_gone(&self, badge: u64) {
        let mut tree = self.tree.borrow_mut();
        tree.views.remove(&badge);
        tree.waiting.retain(|waiting| waiting.view != badge);
    }

    pub(crate) fn mount_gone(&self, badge: u64) {
        self.tree.borrow_mut().mount_sessions.remove(&badge);
        let removed = self
            .tree
            .borrow_mut()
            .mounts
            .extract_if(.., |_, mount| mount.badge == badge)
            .next();
        if let Some((prefix, _)) = removed {
            log_fmt(format_args!("mount {prefix} ended"));
            self.wake_waiting();
        }
    }

    pub(crate) fn open(&self, path: &str) -> Result<Port, FileSystemError> {
        let path = Path::new(path)?;
        let MintedSession { badge, port } = self
            .views
            .create_session()
            .map_err(|_| FileSystemError::NoSpace)?;
        self.tree
            .borrow_mut()
            .views
            .insert(badge.get(), String::from(path.as_str()));
        Ok(port)
    }

    fn wake_waiting(&self) {
        let wakers: Vec<_> = {
            let mut tree = self.tree.borrow_mut();
            let Tree {
                views,
                mounts,
                waiting,
                ..
            } = &mut *tree;
            waiting
                .extract_if(.., |waiting| {
                    views.get(&waiting.view).is_none_or(|scope| {
                        !matches!(route(mounts, scope, &waiting.request), Ok(None))
                    })
                })
                .filter_map(|waiting| waiting.waker.borrow_mut().take())
                .collect()
        };

        for waker in wakers {
            waker.wake();
        }
    }

    fn remove_mount(&self, select: impl Fn(&Mount) -> bool, reason: MountEnded) {
        let removed = self
            .tree
            .borrow_mut()
            .mounts
            .extract_if(.., |_, mount| select(mount))
            .next();
        if let Some((prefix, mount)) = removed {
            end(&prefix, mount, reason);
            self.wake_waiting();
        }
    }

    fn poll_route(
        &self,
        waiting: &Rc<Waiting>,
        cx: &Context<'_>,
    ) -> Poll<Result<ReadyRoute, FileSystemError>> {
        if let Some(error) = waiting.refused.take() {
            return Poll::Ready(Err(error));
        }

        let (outcome, refused) = {
            let mut tree = self.tree.borrow_mut();
            let routed = tree
                .views
                .get(&waiting.view)
                .ok_or(FileSystemError::NoSession)
                .and_then(|scope| route(&tree.mounts, scope, &waiting.request));
            match routed {
                Ok(Some((mount, tail, root_len))) => {
                    let ready = (mount.id, mount.filesystem.clone(), tail, root_len);
                    tree.waiting.retain(|queued| !Rc::ptr_eq(queued, waiting));
                    (Poll::Ready(Ok(ready)), None)
                }

                Err(error) => (Poll::Ready(Err(error)), None),
                Ok(None) => {
                    *waiting.waker.borrow_mut() = Some(cx.waker().clone());
                    let refused = if tree
                        .waiting
                        .iter()
                        .any(|queued| Rc::ptr_eq(queued, waiting))
                    {
                        None
                    } else {
                        enqueue(&mut tree, waiting.clone())
                    };
                    (Poll::Pending, refused)
                }
            }
        };

        if let Some(refused) = refused {
            if Rc::ptr_eq(&refused, waiting) {
                return Poll::Ready(Err(FileSystemError::NoSpace));
            }

            refused.refused.set(Some(FileSystemError::NoSpace));
            let waker = refused.waker.borrow_mut().take();
            if let Some(waker) = waker {
                waker.wake();
            }
        }
        outcome
    }

    async fn answer(
        &self,
        view: u64,
        request: Request,
    ) -> Result<(ReceivedSession<file_system::Protocol>, u32), FileSystemError> {
        let waiting = Rc::new(Waiting {
            view,
            request,
            refused: Cell::new(None),
            waker: RefCell::new(None),
        });
        let _registration = Registration {
            tree: &self.tree,
            waiting: waiting.clone(),
        };

        loop {
            let (id, filesystem, tail, root_len) =
                core::future::poll_fn(|cx| self.poll_route(&waiting, cx)).await?;

            match filesystem.open_root(&tail).await {
                Ok(session) => return Ok((session, root_len)),
                Err(CallError::Remote(error)) => return Err(error),
                Err(CallError::Ipc(IpcError::PeerClosed)) => {
                    self.remove_mount(|mount| mount.id == id, MountEnded::FileSystemGone);
                }

                Err(CallError::Ipc(IpcError::ResourceExhausted)) => {
                    return Err(FileSystemError::NoSpace);
                }

                Err(CallError::Ipc(_)) => return Err(FileSystemError::Unresponsive),
            }
        }
    }
}

impl Drop for Registration<'_> {
    fn drop(&mut self) {
        self.tree
            .borrow_mut()
            .waiting
            .retain(|queued| !Rc::ptr_eq(queued, &self.waiting));
    }
}

#[allow(clippy::unused_async_trait_impl)]
impl namespace::Service<Delivery> for Router {
    fn open_root(
        &self,
        frame: &Delivery,
        path: &str,
    ) -> impl IntoWireValue<Delivery, Read = Result<Session<namespace::Protocol>, FileSystemError>>
    {
        let root = self
            .tree
            .borrow()
            .views
            .get(&frame.badge)
            .cloned()
            .ok_or(FileSystemError::NoSession)?;
        let mut full = [0; PATH_MAX];
        let path = rebase(Path::new(&root)?, Path::new(path)?, &mut full)?;
        self.open(path.as_str())
            .map(kernel_objects::Session::<namespace::Protocol>::from_handle)
    }

    async fn resolve(
        &self,
        frame: &Delivery,
        path: &str,
    ) -> impl IntoWireValue<Delivery, Read = Result<Session<file_system::Protocol>, FileSystemError>>
    {
        self.answer(
            frame.badge,
            Request::Resolve {
                path: String::from(path),
            },
        )
        .await
        .map(|(session, _)| session)
    }

    async fn resolve_pair(
        &self,
        frame: &Delivery,
        from: &str,
        to: &str,
    ) -> impl IntoWireValue<Delivery, Read = Result<ResolvedPair, FileSystemError>> {
        self.answer(
            frame.badge,
            Request::Pair {
                from: String::from(from),
                to: String::from(to),
            },
        )
        .await
        .map(|(session, root_len)| ResolvedPair { session, root_len })
    }
}

impl file_system_mount::Service<Delivery> for Router {
    async fn mount(
        &self,
        frame: &Delivery,
        path: &str,
        filesystem: ReceivedSession<file_system::Protocol>,
    ) -> Result<MountEnded, FileSystemError> {
        let prefix = String::from(Path::new(path)?.as_str());
        let ending = Rc::new(MountEnding::default());
        let displaced = {
            let mut tree = self.tree.borrow_mut();
            if tree.mounts.values().any(|mount| mount.badge == frame.badge) {
                return Err(FileSystemError::AlreadyExists);
            }

            if !tree.mount_sessions.contains(&frame.badge) {
                return Err(FileSystemError::NoSession);
            }

            let id = tree.next_mount;
            tree.next_mount = id.wrapping_add(1);
            let filesystem = PortChannel::client(
                kernel_objects::Session::from_session(filesystem),
                self.reactor.clone(),
            )
            .with_wait(FILE_SYSTEM_OPEN_ROOT_TIMEOUT);
            tree.mounts.insert(
                prefix.clone(),
                Mount {
                    id,
                    badge: frame.badge,
                    filesystem: Rc::new(filesystem),
                    ending: ending.clone(),
                },
            )
        };

        if let Some(displaced) = displaced {
            end(&prefix, displaced, MountEnded::Replaced);
        }
        log_fmt(format_args!("mount {prefix}"));
        self.wake_waiting();
        core::future::poll_fn(|cx| {
            if let Some(reason) = ending.reason.take() {
                return Poll::Ready(Ok(reason));
            }
            *ending.waker.borrow_mut() = Some(cx.waker().clone());
            Poll::Pending
        })
        .await
    }
}

fn enqueue(tree: &mut Tree, waiting: Rc<Waiting>) -> Option<Rc<Waiting>> {
    let in_view = tree
        .waiting
        .iter()
        .filter(|queued| queued.view == waiting.view)
        .count();
    let oldest = (in_view >= VIEW_WAITING_REQUESTS_MAX)
        .then(|| {
            tree.waiting
                .iter()
                .position(|queued| queued.view == waiting.view)
        })
        .flatten();
    if let Some(oldest) = oldest {
        let evicted = tree.waiting.remove(oldest);
        tree.waiting.push(waiting);
        return Some(evicted);
    }

    if tree.waiting.len() < WAITING_REQUESTS_MAX {
        tree.waiting.push(waiting);
        None
    } else {
        Some(waiting)
    }
}

fn end(prefix: &str, mount: Mount, reason: MountEnded) {
    let Mount { ending, .. } = mount;
    ending.reason.set(Some(reason));
    let waker = ending.waker.borrow_mut().take();
    if let Some(waker) = waker {
        waker.wake();
    }
    log_fmt(format_args!("mount {prefix} ended"));
}

fn route<'m>(
    mounts: &'m BTreeMap<String, Mount>,
    scope: &str,
    request: &Request,
) -> Result<Route<'m>, FileSystemError> {
    let scope = Path::new(scope)?;
    match request {
        Request::Resolve { path, .. } => route_path(mounts, scope, path),
        Request::Pair { from, to, .. } => route_pair(mounts, scope, from, to),
    }
}

fn route_path<'m>(
    mounts: &'m BTreeMap<String, Mount>,
    scope: Path<'_>,
    path: &str,
) -> Result<Route<'m>, FileSystemError> {
    let mut full = [0; PATH_MAX];
    let full = rebase(scope, Path::new(path)?, &mut full)?;
    let Some((prefix, mount)) = covering(mounts, scope, full) else {
        return uncovered(mounts, scope, full);
    };
    let tail = full.strip_prefix(prefix).ok_or(FileSystemError::NotFound)?;
    Ok(Some((mount, String::from(tail.as_str()), 0)))
}

fn route_pair<'m>(
    mounts: &'m BTreeMap<String, Mount>,
    scope: Path<'_>,
    from: &str,
    to: &str,
) -> Result<Route<'m>, FileSystemError> {
    let (mut from_full, mut to_full) = ([0; PATH_MAX], [0; PATH_MAX]);
    let from = rebase(scope, Path::new(from)?, &mut from_full)?;
    let to = rebase(scope, Path::new(to)?, &mut to_full)?;
    let (from_mount, to_mount) = (covering(mounts, scope, from), covering(mounts, scope, to));
    let (Some((prefix, mount)), Some((other, _))) = (from_mount, to_mount) else {
        for (path, found) in [(from, from_mount), (to, to_mount)] {
            if found.is_none() {
                uncovered(mounts, scope, path)?;
            }
        }
        return Ok(None);
    };
    if prefix != other {
        return Err(FileSystemError::CrossMount);
    }

    // Общий каталог пары может лежать выше корня монтирования, и сессия не поднимается над ним.
    let common = common_directory(from, to)?;
    let root = common.strip_prefix(prefix).map_or(prefix, |_| common);
    let tail = root.strip_prefix(prefix).ok_or(FileSystemError::NotFound)?;
    let seen = root.strip_prefix(scope).ok_or(FileSystemError::NotFound)?;
    let root_len = u32::try_from(seen.as_str().len()).map_err(|_| FileSystemError::InvalidPath)?;
    Ok(Some((mount, String::from(tail.as_str()), root_len)))
}

fn covering<'m>(
    mounts: &'m BTreeMap<String, Mount>,
    scope: Path<'_>,
    path: Path<'_>,
) -> Option<(Path<'m>, &'m Mount)> {
    // Вид использует монтирования на своей области и под ней.
    mounts
        .iter()
        .filter_map(|(prefix, mount)| Some((Path::new(prefix).ok()?, mount)))
        .filter(|(prefix, _)| {
            prefix.strip_prefix(scope).is_some() && path.strip_prefix(*prefix).is_some()
        })
        .max_by_key(|(prefix, _)| prefix.as_str().len())
}

fn uncovered<'m>(
    mounts: &BTreeMap<String, Mount>,
    scope: Path<'_>,
    path: Path<'_>,
) -> Result<Route<'m>, FileSystemError> {
    let above_mount = mounts
        .keys()
        .filter_map(|prefix| Path::new(prefix).ok())
        .any(|prefix| prefix.strip_prefix(scope).is_some() && prefix.strip_prefix(path).is_some());
    if above_mount {
        return Err(FileSystemError::NotFound);
    }

    Ok(None)
}
