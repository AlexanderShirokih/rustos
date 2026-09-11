//! Дерево имён маршрутизатора: области сессий, монтирования по префиксам и ожидающие запросы
//! видов.

use alloc::{collections::BTreeMap, string::String, sync::Arc, vec::Vec};
use core::sync::atomic::AtomicU64;

use component_startup::{MintedSession, SessionError, mint_session};
use filesystem_abi::{
    FileSystemError, PATH_MAX, Path, ResolvedPair, common_directory, file_system, namespace,
    namespace_admin, rebase,
};
use ipc::wire::{IpcError, Session, Str};
use kernel_objects::{Delivery, Port, ReceivedHandle, SessionTransport, kind};
use runtime::{Mutex, Timeout, log_fmt};

const OPEN_ROOT_TIMEOUT: Timeout = Timeout::from_secs(1);
const WAITING_REQUESTS_MAX: usize = 64;
const VIEW_WAITING_REQUESTS_MAX: usize = 8;

type Debt = ReceivedHandle<kind::Reply>;
type ReceivedSession = Session<file_system::Protocol, ReceivedHandle<kind::Port>>;
type FileSystemClient = Arc<Mutex<file_system::Client<SessionTransport>>>;
type Route<'m> = Option<(&'m Mount, String, u32)>;

static VIEW_BADGES: AtomicU64 = AtomicU64::new(0);
static ADMIN_BADGES: AtomicU64 = AtomicU64::new(0);
static TREE: Mutex<Tree> = Mutex::new(Tree {
    views: BTreeMap::new(),
    admins: BTreeMap::new(),
    mounts: BTreeMap::new(),
    waiting: Vec::new(),
    next_mount: 0,
});

struct Tree {
    views: BTreeMap<u64, String>,
    admins: BTreeMap<u64, String>,
    mounts: BTreeMap<String, Mount>,
    waiting: Vec<Waiting>,
    next_mount: u64,
}

struct Mount {
    id: u64,
    badge: u64,
    filesystem: FileSystemClient,
    call: namespace_admin::mount::Reply<Debt>,
}

struct Waiting {
    view: u64,
    request: Pending,
}

enum Pending {
    Resolve {
        path: String,
        reply: namespace::resolve::Reply<Debt>,
    },
    Pair {
        from: String,
        to: String,
        reply: namespace::resolve_pair::Reply<Debt>,
    },
}

pub(crate) struct Router;

impl Pending {
    fn reply(self, answer: Result<(ReceivedSession, u32), FileSystemError>) {
        // Недоставленный ответ закрывает capability сессии, и файловая система снимает её по
        // уходу значка.
        match self {
            Self::Resolve { reply, .. } => {
                let _ = reply.send(answer.map(|(session, _)| session));
            }
            Self::Pair { reply, .. } => {
                let _ = reply
                    .send(answer.map(|(session, root_len)| ResolvedPair { session, root_len }));
            }
        }
    }
}

pub(crate) fn open_view(export: &Port, scope: &str) -> Result<Port, SessionError> {
    let scope = if scope.is_empty() { "/" } else { scope };
    open(export, &VIEW_BADGES, scope, |tree| &mut tree.views)
}

pub(crate) fn open_admin(export: &Port, scope: &str) -> Result<Port, SessionError> {
    if scope.is_empty() {
        return Err(SessionError::Refused);
    }
    open(export, &ADMIN_BADGES, scope, |tree| &mut tree.admins)
}

pub(crate) fn view_gone(badge: u64) {
    let mut tree = TREE.lock();
    tree.views.remove(&badge);
    // Долг, разрушенный без ответа, даёт ушедшему клиенту `PeerClosed`.
    tree.waiting.retain(|waiting| waiting.view != badge);
}

pub(crate) fn admin_gone(badge: u64) {
    TREE.lock().admins.remove(&badge);
    // Если запись этой сессии вытеснена, поиск по значку её не находит и монтирование другой
    // сессии не снимает.
    remove_mount(|mount| mount.badge == badge);
}

impl namespace::Service<Delivery> for Router {
    fn resolve(
        &mut self,
        frame: &Delivery,
        path: Str<'_, PATH_MAX>,
        reply: namespace::resolve::Reply<Debt>,
    ) {
        let path = String::from(path.as_str());
        answer(frame.badge, Pending::Resolve { path, reply });
    }

    fn resolve_pair(
        &mut self,
        frame: &Delivery,
        from: Str<'_, PATH_MAX>,
        to: Str<'_, PATH_MAX>,
        reply: namespace::resolve_pair::Reply<Debt>,
    ) {
        let (from, to) = (String::from(from.as_str()), String::from(to.as_str()));
        answer(frame.badge, Pending::Pair { from, to, reply });
    }
}

impl namespace_admin::Service<Delivery> for Router {
    fn mount(
        &mut self,
        frame: &Delivery,
        filesystem: ReceivedSession,
        call: namespace_admin::mount::Reply<Debt>,
    ) {
        let badge = frame.badge;
        let mut tree = TREE.lock();
        let mounted = tree.mounts.values().any(|mount| mount.badge == badge);
        let Some(prefix) = tree.admins.get(&badge).filter(|_| !mounted).cloned() else {
            drop(tree);
            // Отказанное монтирование закрывает переданную сессию.
            let refusal = if mounted {
                FileSystemError::AlreadyExists
            } else {
                FileSystemError::NoSession
            };
            let _ = call.send(Err::<(), _>(refusal));
            return;
        };

        let id = tree.next_mount;
        tree.next_mount = id.wrapping_add(1);
        let client = kernel_objects::Session::from_session(filesystem).client();
        let filesystem = Arc::new(Mutex::new(client.with_wait(OPEN_ROOT_TIMEOUT)));
        let displaced = tree.mounts.insert(
            prefix.clone(),
            Mount {
                id,
                badge,
                filesystem,
                call,
            },
        );
        let covered = take_covered(&mut tree);
        drop(tree);

        if let Some(displaced) = displaced {
            end(&prefix, displaced);
        }
        log_fmt(format_args!("mount {prefix}"));
        for Waiting { view, request } in covered {
            answer(view, request);
        }
    }
}

fn open(
    export: &Port,
    next_badge: &AtomicU64,
    scope: &str,
    sessions: impl FnOnce(&mut Tree) -> &mut BTreeMap<u64, String>,
) -> Result<Port, SessionError> {
    let scope = Path::new(scope).map_err(|_| SessionError::Refused)?;
    let MintedSession { badge, port } =
        mint_session(export, next_badge).map_err(|_| SessionError::NoSpace)?;
    // Запись сессии создаётся до передачи её capability импортёру.
    sessions(&mut TREE.lock()).insert(badge.get(), String::from(scope.as_str()));
    Ok(port)
}

fn answer(view: u64, request: Pending) {
    loop {
        let mut tree = TREE.lock();
        let routed = match tree.views.get(&view) {
            Some(scope) => route(&tree.mounts, scope, &request),
            None => Err(FileSystemError::NoSession),
        };
        let (id, filesystem, tail, root_len) = match routed {
            Ok(Some((mount, tail, root_len))) => {
                (mount.id, Arc::clone(&mount.filesystem), tail, root_len)
            }
            Ok(None) => {
                if let Some(refused) = enqueue(&mut tree, view, request) {
                    refused.reply(Err(FileSystemError::NoSpace));
                }
                return;
            }
            Err(error) => return request.reply(Err(error)),
        };
        drop(tree);

        let opened = match Str::new(&tail) {
            Some(tail) => filesystem.lock().open_root(tail),
            None => Ok(Err(FileSystemError::InvalidPath)),
        };
        match opened {
            Ok(opened) => return request.reply(opened.map(|session| (session, root_len))),
            // Монтирование с закрытым портом снимается, и запрос ждёт следующего.
            Err(IpcError::PeerClosed) => remove_mount(|mount| mount.id == id),
            // Ресурс исчерпан у самого маршрутизатора, не у файловой системы.
            Err(IpcError::ResourceExhausted) => {
                return request.reply(Err(FileSystemError::NoSpace));
            }
            Err(_) => return request.reply(Err(FileSystemError::Unresponsive)),
        }
    }
}

fn enqueue(tree: &mut Tree, view: u64, request: Pending) -> Option<Pending> {
    let in_view = tree
        .waiting
        .iter()
        .filter(|waiting| waiting.view == view)
        .count();
    let oldest = (in_view >= VIEW_WAITING_REQUESTS_MAX)
        .then(|| tree.waiting.iter().position(|waiting| waiting.view == view))
        .flatten();
    if let Some(oldest) = oldest {
        // Замена самого старого запроса вида освобождает его слот новому без изменения общей
        // очереди.
        let evicted = tree.waiting.remove(oldest);
        tree.waiting.push(Waiting { view, request });
        return Some(evicted.request);
    }
    if tree.waiting.len() < WAITING_REQUESTS_MAX {
        tree.waiting.push(Waiting { view, request });
        return None;
    }
    Some(request)
}

fn route<'m>(
    mounts: &'m BTreeMap<String, Mount>,
    scope: &str,
    request: &Pending,
) -> Result<Route<'m>, FileSystemError> {
    let scope = Path::new(scope)?;
    match request {
        Pending::Resolve { path, .. } => route_path(mounts, scope, path),
        Pending::Pair { from, to, .. } => route_pair(mounts, scope, from, to),
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
        return Ok(None);
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
    let (Some((prefix, mount)), Some((other, _))) =
        (covering(mounts, scope, from), covering(mounts, scope, to))
    else {
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

fn take_covered(tree: &mut Tree) -> Vec<Waiting> {
    let (views, mounts) = (&tree.views, &tree.mounts);
    tree.waiting
        .extract_if(.., |waiting| {
            views
                .get(&waiting.view)
                .is_none_or(|scope| !matches!(route(mounts, scope, &waiting.request), Ok(None)))
        })
        .collect()
}

fn remove_mount(select: impl Fn(&Mount) -> bool) {
    let ended = TREE
        .lock()
        .mounts
        .extract_if(.., |_, mount| select(mount))
        .next();
    if let Some((prefix, mount)) = ended {
        end(&prefix, mount);
    }
}

fn end(prefix: &str, mount: Mount) {
    // Ответ отправляется после снятия. Запрос, отправленный после возврата `mount`, снятое
    // монтирование не использует.
    let _ = mount.call.send(Ok::<(), FileSystemError>(()));
    log_fmt(format_args!("mount {prefix} ended"));
}
