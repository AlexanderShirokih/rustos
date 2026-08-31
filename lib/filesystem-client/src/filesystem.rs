//! Разбор пути в сессию, операции над узлами и обход дерева.

use alloc::{
    string::{String, ToString},
    vec::Vec,
};

use filesystem_abi::{
    AccessMode, FileSystemError, NodeInfo, OpenFlags, Opened, Path, SEPARATOR, namespace,
};
use ipc::{
    Timeout,
    wire::{Cap, Str},
};

use crate::{
    cache::SessionCache, environment::Environment, error::Result, listing::Listing, node::OpenNode,
    route, session::Session,
};

const SESSIONS: usize = 4;

const WAIT: Timeout = Timeout::from_secs(2);

/// Файловая подсистема, с которой работает программа.
pub struct FileSystem<E: Environment> {
    environment: E,
    sessions: Sessions<E>,
    wait: Timeout,
}

enum Sessions<E: Environment> {
    Routed {
        namespace: namespace::Client<E::Transport>,
        cache: SessionCache<E>,
    },
    Window(Option<Window<E>>),
}

enum Window<E: Environment> {
    Pending(E::Transport),
    Open(Session<E>),
}

struct Pending {
    path: String,
    emptied: bool,
}

impl<E: Environment> FileSystem<E> {
    /// Фасад поверх маршрутизатора имён. Маршрутизатор разбирает путь, фасад хранит
    /// сессии разобранных каталогов.
    pub fn routed(environment: E, namespace: E::Transport) -> Self {
        Self {
            environment,
            sessions: Sessions::Routed {
                namespace: namespace::Client::new(namespace).with_wait(WAIT),
                cache: SessionCache::new(SESSIONS),
            },
            wait: WAIT,
        }
    }

    /// Фасад поверх выданной сессии: путь отсчитывается от её корня.
    pub fn windowed(environment: E, session: E::Transport) -> Self {
        Self {
            environment,
            sessions: Sessions::Window(Some(Window::Pending(session))),
            wait: WAIT,
        }
    }

    /// Задаёт срок ожидания ответа на вызов. Срок действует на сессии, открытые после него.
    #[must_use]
    pub fn with_wait(mut self, wait: Timeout) -> Self {
        self.wait = wait;
        if let Sessions::Routed { namespace, cache } = self.sessions {
            self.sessions = Sessions::Routed {
                namespace: namespace.with_wait(wait),
                cache,
            };
        }
        self
    }

    /// Открывает узел по пути.
    pub fn open(
        &mut self,
        path: &str,
        mode: AccessMode,
        flags: OpenFlags,
    ) -> Result<OpenNode<'_, E>> {
        let path = Path::new(path)?;
        let opened = self.at_path(path, |session, root| {
            Ok(session
                .client()
                .open(route::within(path, root)?, mode, flags)??)
        })?;
        Ok(OpenNode::new(self.session(), opened))
    }

    /// Создаёт каталог по пути.
    pub fn create_directory(&mut self, path: &str) -> Result<()> {
        self.open(
            path,
            AccessMode::Read,
            OpenFlags::CREATE | OpenFlags::DIRECTORY,
        )?;
        Ok(())
    }

    /// Метаданные узла по пути.
    pub fn metadata(&mut self, path: &str) -> Result<NodeInfo> {
        let node = self.open(path, AccessMode::Read, OpenFlags::NONE)?;
        Ok(NodeInfo {
            kind: node.kind(),
            size: node.size(),
        })
    }

    /// Обход каталога по пути.
    pub fn read_directory(&mut self, path: &str) -> Result<Listing<'_, E>> {
        self.open(path, AccessMode::Read, OpenFlags::DIRECTORY)?
            .entries()
    }

    /// Снимает имя узла. Каталог снимается пустым.
    pub fn remove(&mut self, path: &str) -> Result<()> {
        let path = Path::new(path)?;
        self.at_path(path, |session, root| {
            Ok(session.client().unlink(route::within(path, root)?)??)
        })?;
        Ok(())
    }

    /// Удаляет узел вместе с поддеревом внутри его монтирования.
    pub fn remove_tree(&mut self, path: &str) -> Result<()> {
        let path = Path::new(path)?;
        self.at_path(path, |session, root| {
            remove_tree_in(session, route::within(path, root)?.as_str())
        })?;
        Ok(())
    }

    /// Переименовывает узел в пределах одного монтирования.
    pub fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        let from = Path::new(from)?;
        let to = Path::new(to)?;
        self.at_root(route::common_directory(from, to)?, &|session, root| {
            Ok(session
                .client()
                .rename(route::within(from, root)?, route::within(to, root)?)??)
        })?;
        Ok(())
    }

    fn at_path<T>(
        &mut self,
        path: Path<'_>,
        operation: impl Fn(&Session<E>, Path<'_>) -> Result<T>,
    ) -> Result<T> {
        let outcome = self.at_root(route::directory_of(path), &operation);
        if path.is_root() || !self.asks_the_router() {
            return outcome;
        }
        match outcome {
            // Путь, равный корню монтирования, в родительской файловой системе имени не
            // имеет, и маршрутизатор разбирает его сам.
            Err(failure) if failure.name_missing() => self.at_root(path, &operation),
            outcome => outcome,
        }
    }

    fn at_root<T>(
        &mut self,
        directory: Path<'_>,
        operation: &impl Fn(&Session<E>, Path<'_>) -> Result<T>,
    ) -> Result<T> {
        let Self {
            environment,
            sessions,
            wait,
        } = self;
        let (namespace, cache) = match sessions {
            Sessions::Window(window) => {
                open_window(window, environment, *wait)?;

                return operation(opened_window(window.as_ref())?, Path::ROOT);
            }
            Sessions::Routed { namespace, cache } => (namespace, cache),
        };
        if let Some(session) = cache.promote(directory) {
            match operation(session, directory) {
                Err(failure) if failure.peer_gone() || failure.name_missing() => {
                    cache.forget_head();
                }
                outcome => return outcome,
            }
        }
        let resolved = resolve::<E>(namespace, directory)?;
        let session = Session::establish(environment, resolved, *wait)?;

        let outcome = operation(&session, directory);
        cache.insert(directory, session);
        outcome
    }

    fn asks_the_router(&self) -> bool {
        matches!(self.sessions, Sessions::Routed { .. })
    }

    fn session(&self) -> &Session<E> {
        match &self.sessions {
            Sessions::Window(window) => {
                opened_window(window.as_ref()).expect("the operation opened the window session")
            }
            Sessions::Routed { cache, .. } => {
                cache.head().expect("the operation left its session cached")
            }
        }
    }
}

fn open_window<E: Environment>(
    window: &mut Option<Window<E>>,
    environment: &E,
    wait: Timeout,
) -> Result<()> {
    if !matches!(window, Some(Window::Pending(_))) {
        return Ok(());
    }
    let Some(Window::Pending(transport)) = window.take() else {
        return Err(FileSystemError::NoSession.into());
    };

    *window = Some(Window::Open(Session::over(environment, transport, wait)?));
    Ok(())
}

fn opened_window<E: Environment>(window: Option<&Window<E>>) -> Result<&Session<E>> {
    match window {
        Some(Window::Open(session)) => Ok(session),
        _ => Err(FileSystemError::NoSession.into()),
    }
}

fn resolve<E: Environment>(
    namespace: &namespace::Client<E::Transport>,
    directory: Path<'_>,
) -> Result<Cap> {
    let path = Str::new(directory.as_str()).ok_or(FileSystemError::InvalidPath)?;
    Ok(namespace.resolve(path)??)
}

fn remove_tree_in<E: Environment>(session: &Session<E>, root: &str) -> Result<()> {
    // Корень сессии имени в ней не имеет, и снять его нечем.
    if Path::new(root)?.is_root() {
        return Err(FileSystemError::InvalidPath.into());
    }

    let mut pending = Vec::new();
    pending.push(Pending {
        path: root.to_string(),
        emptied: false,
    });
    while let Some(item) = pending.pop() {
        let path = Path::new(&item.path)?;
        if item.emptied {
            unlink(session, path)?;
            continue;
        }

        let opened = open(session, path, OpenFlags::NONE)?;
        if !opened.kind.is_directory() {
            drop(OpenNode::new(session, opened));
            unlink(session, path)?;
            continue;
        }

        // Позиция перечисления переживает удаление только до первого снятого имени.
        let mut children = Vec::new();
        let mut listing = OpenNode::new(session, opened).entries()?;
        while let Some(entry) = listing.next_entry()? {
            children.push(child(path, entry.name)?);
        }
        drop(listing);

        pending.push(Pending {
            path: item.path,
            emptied: true,
        });
        pending.extend(children.into_iter().map(|path| Pending {
            path,
            emptied: false,
        }));
    }
    Ok(())
}

fn open<E: Environment>(session: &Session<E>, path: Path<'_>, flags: OpenFlags) -> Result<Opened> {
    let name = Str::new(path.as_str()).ok_or(FileSystemError::InvalidPath)?;
    Ok(session.client().open(name, AccessMode::Read, flags)??)
}

fn unlink<E: Environment>(session: &Session<E>, path: Path<'_>) -> Result<()> {
    let name = Str::new(path.as_str()).ok_or(FileSystemError::InvalidPath)?;
    session.client().unlink(name)??;
    Ok(())
}

fn child(directory: Path<'_>, name: &str) -> Result<String> {
    let base = if directory.is_root() {
        ""
    } else {
        directory.as_str()
    };
    let mut path = String::new();
    path.try_reserve_exact(base.len() + SEPARATOR.len_utf8() + name.len())
        .map_err(|_| FileSystemError::NoSpace)?;
    path.push_str(base);
    path.push(SEPARATOR);
    path.push_str(name);
    Path::new(&path)?;
    Ok(path)
}
