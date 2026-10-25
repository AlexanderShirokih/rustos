//! Разбор пути в сессию, операции над узлами и обход дерева.

use alloc::{
    string::{String, ToString},
    vec::Vec,
};

use filesystem_abi::{
    AccessMode, FileSystemError, IO_TRANSFER_MAX, NodeDescriptor, NodeInfo, NodeKind, OpenFlags,
    Opened, Path, SEPARATOR, file_system, namespace,
};
use ipc::{Timeout, wire::IpcError};
use syscall::kind;

use crate::{
    cache::SessionCache,
    environment::Environment,
    error::Result,
    listing::{Listing, Walk},
    node::OpenNode,
    route,
    session::Session,
};

const SESSIONS: usize = 4;

const WAIT: Timeout = Timeout::from_secs(2);

/// Файловая подсистема, с которой работает программа. Фасад держит сессии разобранных каталогов
/// и закрывает их вместе с собой.
pub struct FileSystem<E: Environment> {
    environment: E,
    namespace: namespace::Client<E::Transport>,
    cache: SessionCache<E>,
    wait: Timeout,
}

struct Pending {
    path: String,
    kind: NodeKind,
    emptied: bool,
}

impl<E: Environment> FileSystem<E> {
    /// Фасад поверх маршрутизатора имён. Маршрутизатор разбирает путь, фасад хранит
    /// сессии разобранных каталогов.
    pub fn with_environment(environment: E, namespace: E::Transport) -> Self {
        Self {
            environment,
            namespace: namespace::Client::new(namespace).with_wait(WAIT),
            cache: SessionCache::new(SESSIONS),
            wait: WAIT,
        }
    }

    /// Задаёт срок ожидания ответа на вызов. Срок действует на сессии, открытые после него.
    #[must_use]
    pub fn with_wait(mut self, wait: Timeout) -> Self {
        self.wait = wait;
        self.namespace = self.namespace.with_wait(wait);
        self
    }

    /// Открывает узел по пути. `mode` задаёт режим доступа, `flags` - действия над деревом имён.
    /// Узел получает отдельную сессию, и она закрывается вместе с [`OpenNode`].
    pub fn open(
        &mut self,
        path: Path<'_>,
        mode: AccessMode,
        flags: OpenFlags,
    ) -> Result<OpenNode<E>> {
        let wait = self.wait;
        self.at_path(path, |environment, session, root| {
            open_owned(
                environment,
                session,
                route::within(path, root)?,
                mode,
                flags,
                wait,
            )
        })
    }

    /// Создаёт каталог по пути. Существующий каталог отказом не отвечает, а файл под этим именем
    /// возвращает `NotADirectory`.
    pub fn create_directory(&mut self, path: Path<'_>) -> Result<()> {
        self.at_path(path, |_, session, root| {
            let opened = open_in(
                session,
                route::within(path, root)?,
                OpenFlags::CREATE | OpenFlags::DIRECTORY,
            )?;
            close_in(session, opened.fd);
            Ok(())
        })
    }

    /// Метаданные узла по пути.
    pub fn metadata(&mut self, path: Path<'_>) -> Result<NodeInfo> {
        self.at_path(path, |_, session, root| {
            let opened = open_in(session, route::within(path, root)?, OpenFlags::NONE)?;
            close_in(session, opened.fd);
            Ok(opened.info)
        })
    }

    /// Обход каталога по пути. Обход получает отдельную сессию, файл по этому пути возвращает
    /// `NotADirectory`.
    pub fn read_directory(&mut self, path: Path<'_>) -> Result<Listing<E>> {
        self.open(path, AccessMode::Read, OpenFlags::DIRECTORY)?
            .entries()
    }

    /// Снимает имя файла. Каталог по этому пути возвращает `IsADirectory`.
    pub fn remove_file(&mut self, path: Path<'_>) -> Result<()> {
        self.remove_node(path, NodeKind::File)
    }

    /// Снимает имя пустого каталога. Файл по этому пути возвращает `NotADirectory`.
    pub fn remove_directory(&mut self, path: Path<'_>) -> Result<()> {
        self.remove_node(path, NodeKind::Directory)
    }

    /// Удаляет каталог вместе с поддеревом внутри его монтирования. Файл по этому пути
    /// возвращает `NotADirectory`, корень сессии - `InvalidPath`. Отказ посреди обхода
    /// оставляет снятой уже пройденную часть поддерева.
    pub fn remove_tree(&mut self, path: Path<'_>) -> Result<()> {
        self.at_path(path, |_, session, root| {
            remove_tree_in(session, route::within(path, root)?.as_str())
        })
    }

    /// Переименовывает узел в пределах одного монтирования. Пути под разными монтированиями
    /// возвращают `CrossMount`, и файловая система при этом не вызывается.
    pub fn rename(&mut self, from: Path<'_>, to: Path<'_>) -> Result<()> {
        let Self {
            environment,
            namespace,
            cache: _,
            wait,
        } = self;
        let pair = namespace.resolve_pair(from.as_str(), to.as_str()).wait()?;
        let (from, to) = scoped(from, to, pair.root_len)?;
        rename_in(
            &Session::establish(environment, pair.session, *wait)?,
            from,
            to,
        )
    }

    /// Копирует содержимое файла `from` в `to` и возвращает число перенесённых байт. Назначение
    /// создаётся либо усекается. Байты проходят через клиента порциями в пределах файловой
    /// операции. Каталог по пути `from` возвращает `IsADirectory`, и назначение при этом
    /// не открывается.
    pub fn copy(&mut self, from: Path<'_>, to: Path<'_>) -> Result<u64> {
        let mut source = self.open(from, AccessMode::Read, OpenFlags::NONE)?;
        if source.kind().is_directory() {
            return Err(FileSystemError::IsADirectory.into());
        }

        let mut target = self.open(
            to,
            AccessMode::Write,
            OpenFlags::CREATE | OpenFlags::TRUNCATE,
        )?;

        transfer(&mut source, &mut target)
    }

    fn remove_node(&mut self, path: Path<'_>, expected: NodeKind) -> Result<()> {
        self.at_path(path, |_, session, root| {
            unlink(session, route::within(path, root)?, expected)
        })
    }

    fn at_path<T>(
        &mut self,
        path: Path<'_>,
        operation: impl Fn(&E, &Session<E>, Path<'_>) -> Result<T>,
    ) -> Result<T> {
        let outcome = self.at_root(path.directory(), &operation);
        if path.is_root() {
            return outcome;
        }
        match outcome {
            // Путь, равный корню монтирования, в родительской файловой системе имени не
            // имеет, и маршрутизатор сам разбирает его.
            Err(failure) if failure.name_missing() => self.at_root(path, &operation),
            outcome => outcome,
        }
    }

    fn at_root<T>(
        &mut self,
        directory: Path<'_>,
        operation: &impl Fn(&E, &Session<E>, Path<'_>) -> Result<T>,
    ) -> Result<T> {
        let Self {
            environment,
            namespace,
            cache,
            wait,
        } = self;
        if let Some(session) = cache.promote(directory) {
            match operation(environment, session, directory) {
                Err(failure) if failure.peer_gone() || failure.name_missing() => {
                    cache.forget_head();
                }
                outcome => return outcome,
            }
        }
        let resolved = resolve::<E>(namespace, directory)?;
        let session = Session::establish(environment, resolved, *wait)?;

        let outcome = operation(environment, &session, directory);
        cache.insert(directory, session);
        outcome
    }
}

fn resolve<E: Environment>(
    namespace: &namespace::Client<E::Transport>,
    directory: Path<'_>,
) -> Result<ipc::wire::Session<file_system::Protocol, E::Handle<kind::Port>>> {
    Ok(namespace.resolve(directory.as_str()).wait()?)
}

fn scoped<'a, 'b>(from: Path<'a>, to: Path<'b>, root_len: u32) -> Result<(Path<'a>, Path<'b>)> {
    let len = usize::try_from(root_len).map_err(|_| IpcError::InvalidValue)?;
    let head = from.as_str().get(..len).ok_or(IpcError::InvalidValue)?;
    let tail = to.as_str().get(..len).ok_or(IpcError::InvalidValue)?;
    if head != tail {
        return Err(IpcError::InvalidValue.into());
    }
    // Корень совпал с обеими сторонами по границе компонента: `/` подходит любому пути.
    let first = from
        .strip_prefix(Path::new(head).map_err(|_| IpcError::InvalidValue)?)
        .ok_or(IpcError::InvalidValue)?;
    let second = to
        .strip_prefix(Path::new(tail).map_err(|_| IpcError::InvalidValue)?)
        .ok_or(IpcError::InvalidValue)?;
    Ok((first, second))
}

fn rename_in<E: Environment>(session: &Session<E>, from: Path<'_>, to: Path<'_>) -> Result<()> {
    session.client().rename(from.as_str(), to.as_str()).wait()?;
    Ok(())
}

fn open_owned<E: Environment>(
    environment: &E,
    session: &Session<E>,
    within: Path<'_>,
    mode: AccessMode,
    flags: OpenFlags,
    wait: Timeout,
) -> Result<OpenNode<E>> {
    let (directory, leaf) = route::split_scope(within)?;
    let scoped = session.client().open_root(directory.as_str()).wait()?;
    let scoped = Session::establish(environment, scoped, wait)?;
    let opened = scoped.client().open(leaf.as_str(), mode, flags).wait()?;
    Ok(OpenNode::new(scoped, opened))
}

fn transfer<E: Environment>(source: &mut OpenNode<E>, target: &mut OpenNode<E>) -> Result<u64> {
    let len = IO_TRANSFER_MAX as usize;
    let mut chunk = Vec::new();
    chunk
        .try_reserve_exact(len)
        .map_err(|_| FileSystemError::NoSpace)?;
    chunk.resize(len, 0);

    let mut copied = 0u64;
    loop {
        let count = source.read(&mut chunk)?;
        if count == 0 {
            return Ok(copied);
        }

        let mut rest = chunk.get(..count).ok_or(FileSystemError::BufferTooSmall)?;
        while !rest.is_empty() {
            // Повтор записи, не принявшей ни одного байта, копирование не продвинет.
            let written = target.write(rest)?;
            if written == 0 {
                return Err(FileSystemError::NoSpace.into());
            }
            rest = rest.get(written..).unwrap_or_default();
        }

        copied = copied.saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
    }
}

fn remove_tree_in<E: Environment>(session: &Session<E>, root: &str) -> Result<()> {
    // Корень сессии имени в ней не имеет, и снять его нечем.
    if Path::new(root)?.is_root() {
        return Err(FileSystemError::InvalidPath.into());
    }

    let mut pending = Vec::new();
    pending.push(Pending {
        path: root.to_string(),
        kind: NodeKind::Directory,
        emptied: false,
    });
    while let Some(item) = pending.pop() {
        let path = Path::new(&item.path)?;
        if item.emptied || !item.kind.is_directory() {
            unlink(session, path, item.kind)?;
            continue;
        }

        // Флаг каталога требует его вида у корня обхода, а у дочернего узла - совпадения с
        // видом, который вернул обход.
        let opened = open_in(session, path, OpenFlags::DIRECTORY)?;
        // Позиция перечисления остаётся действительной при удалении только до первого снятого
        // имени.
        let mut children = Vec::new();
        let listed = children_of(session, opened.fd, path, &mut children);
        close_in(session, opened.fd);
        listed?;

        pending.push(Pending {
            path: item.path,
            kind: NodeKind::Directory,
            emptied: true,
        });
        pending.extend(children.into_iter().map(|(path, kind)| Pending {
            path,
            kind,
            emptied: false,
        }));
    }
    Ok(())
}

fn children_of<E: Environment>(
    session: &Session<E>,
    descriptor: NodeDescriptor,
    directory: Path<'_>,
    children: &mut Vec<(String, NodeKind)>,
) -> Result<()> {
    let mut walk = Walk::new(IO_TRANSFER_MAX as usize)?;
    while let Some(entry) = walk.next_entry(session, descriptor)? {
        children.push((child(directory, entry.name)?, entry.kind));
    }

    Ok(())
}

fn open_in<E: Environment>(
    session: &Session<E>,
    path: Path<'_>,
    flags: OpenFlags,
) -> Result<Opened> {
    Ok(session
        .client()
        .open(path.as_str(), AccessMode::Read, flags)
        .wait()?)
}

fn close_in<E: Environment>(session: &Session<E>, descriptor: NodeDescriptor) {
    let _ = session.client().close(descriptor).wait();
}

fn unlink<E: Environment>(session: &Session<E>, path: Path<'_>, expected: NodeKind) -> Result<()> {
    session.client().unlink(path.as_str(), expected).wait()?;
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
