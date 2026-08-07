//! Состояние клиентов сервера: таблица сессий и открытые узлы внутри каждой.

use alloc::{
    collections::{BTreeMap, btree_map::Entry},
    vec::Vec,
};
use core::num::NonZeroU16;

use filesystem_abi::{AccessMode, FileSystemError, NodeDescriptor};

use crate::node::NodeId;

const OPEN_NODE_LIMIT: usize = 64;

/// Ключ, по которому сервер узнаёт клиента своей сессии. Значение назначает
/// сервер.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClientId(u64);

/// Узел, открытый сессией: его идентичность вместе с режимом доступа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenNode {
    pub node: NodeId,
    pub mode: AccessMode,
}

struct DescriptorSlot {
    generation: u16,
    node: Option<OpenNode>,
}

impl DescriptorSlot {
    const fn fresh() -> Self {
        Self {
            generation: 0,
            node: None,
        }
    }

    fn is_vacant(&self) -> bool {
        // Исчерпанное поколение выводит слот из оборота: иначе дескриптор повторит значение
        // жившего когда-то.
        self.node.is_none() && self.generation < u16::MAX
    }

    fn occupy(&mut self, slot: u32, open_node: OpenNode) -> NodeDescriptor {
        let issued = NonZeroU16::MIN.saturating_add(self.generation);
        self.generation = issued.get();
        self.node = Some(open_node);
        NodeDescriptor::pack(issued, slot)
    }

    fn get(&self, fd: NodeDescriptor) -> Option<OpenNode> {
        (self.generation == fd.generation())
            .then_some(self.node)
            .flatten()
    }
}

/// Состояние одного клиента: его открытые узлы и прикрепление `A`, которое
/// вешает на клиента сервер.
pub struct Session<A> {
    attachment: A,
    open_nodes: Vec<DescriptorSlot>,
}

/// Сессии по ключу клиента.
pub struct SessionTable<A> {
    sessions: BTreeMap<ClientId, Session<A>>,
}

impl ClientId {
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

impl<A> Session<A> {
    pub fn attachment(&self) -> &A {
        &self.attachment
    }

    pub fn attachment_mut(&mut self) -> &mut A {
        &mut self.attachment
    }

    /// Открывает дескриптор, занимая освободившийся слот. Сверх потолка -
    /// `TooManyOpenFiles`. Значение дескриптора несёт поколение слота: отставший
    /// вызов по закрытому дескриптору не попадает в новый узел этого слота.
    pub fn open(&mut self, open_node: OpenNode) -> Result<NodeDescriptor, FileSystemError> {
        if let Some(slot) = self.open_nodes.iter().position(DescriptorSlot::is_vacant) {
            return Ok(self.open_nodes[slot].occupy(slot as u32, open_node));
        }
        if self.open_nodes.len() >= OPEN_NODE_LIMIT {
            return Err(FileSystemError::TooManyOpenFiles);
        }
        self.open_nodes.push(DescriptorSlot::fresh());
        let slot = self.open_nodes.len() - 1;
        Ok(self.open_nodes[slot].occupy(slot as u32, open_node))
    }

    /// Узел, открытый под дескриптором `fd`. Дескриптор вне сессии либо с ушедшим
    /// поколением - `BadDescriptor`.
    pub fn get(&self, fd: NodeDescriptor) -> Result<OpenNode, FileSystemError> {
        self.open_nodes
            .get(fd.slot() as usize)
            .and_then(|slot| slot.get(fd))
            .ok_or(FileSystemError::BadDescriptor)
    }

    /// Закрывает узел, освобождая его слот под выдачу со свежим поколением.
    pub fn close(&mut self, fd: NodeDescriptor) -> Result<OpenNode, FileSystemError> {
        let slot = self
            .open_nodes
            .get_mut(fd.slot() as usize)
            .ok_or(FileSystemError::BadDescriptor)?;
        if slot.generation != fd.generation() {
            return Err(FileSystemError::BadDescriptor);
        }
        slot.node.take().ok_or(FileSystemError::BadDescriptor)
    }
}

impl<A: Default> Default for Session<A> {
    fn default() -> Self {
        Self {
            attachment: A::default(),
            open_nodes: Vec::new(),
        }
    }
}

impl<A> SessionTable<A> {
    pub const fn new() -> Self {
        Self {
            sessions: BTreeMap::new(),
        }
    }

    pub fn get(&self, client: ClientId) -> Option<&Session<A>> {
        self.sessions.get(&client)
    }

    /// Забывает сессию вместе с её открытыми узлами и прикреплением.
    pub fn remove(&mut self, client: ClientId) -> Option<Session<A>> {
        self.sessions.remove(&client)
    }

    /// Сессия клиента. Незнакомый ключ - `NoSession`.
    pub fn get_mut(&mut self, client: ClientId) -> Result<&mut Session<A>, FileSystemError> {
        self.sessions
            .get_mut(&client)
            .ok_or(FileSystemError::NoSession)
    }
}

impl<A: Default> SessionTable<A> {
    /// Заводит сессию клиента с прикреплением по умолчанию. Занятый ключ -
    /// `AlreadyExists`.
    ///
    /// Единственный способ завести сессию. Кадр сессии не заводит: явность отделяет
    /// отставший кадр ушедшего клиента от живой сессии и от убранной, чьё событие
    /// об уходе уже прочитано.
    pub fn open(&mut self, client: ClientId) -> Result<&mut Session<A>, FileSystemError> {
        match self.sessions.entry(client) {
            Entry::Occupied(_) => Err(FileSystemError::AlreadyExists),
            Entry::Vacant(slot) => Ok(slot.insert(Session::default())),
        }
    }
}

impl<A> Default for SessionTable<A> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use filesystem_abi::{AccessMode, FileSystemError, NodeDescriptor};

    use super::{ClientId, OPEN_NODE_LIMIT, OpenNode, Session, SessionTable};
    use crate::node::NodeId;

    const ALICE: ClientId = ClientId::from_raw(1);
    const BOB: ClientId = ClientId::from_raw(2);

    fn open_node(node: u32) -> OpenNode {
        OpenNode {
            node: NodeId::from_raw(node),
            mode: AccessMode::Read,
        }
    }

    fn table() -> SessionTable<()> {
        SessionTable::new()
    }

    fn session(sessions: &mut SessionTable<()>, client: ClientId) -> &mut Session<()> {
        sessions.get_mut(client).expect("established session")
    }

    #[test]
    fn session_appears_only_when_opened() {
        let mut sessions = table();
        assert_eq!(
            sessions.get_mut(ALICE).err(),
            Some(FileSystemError::NoSession)
        );
        sessions.open(ALICE).expect("open");
        assert!(sessions.get(ALICE).is_some());
        assert_eq!(
            sessions.get_mut(BOB).err(),
            Some(FileSystemError::NoSession)
        );
    }

    #[test]
    fn opening_an_established_session_is_rejected() {
        let mut sessions = table();
        sessions.open(ALICE).expect("open");
        assert_eq!(
            sessions.open(ALICE).err(),
            Some(FileSystemError::AlreadyExists)
        );
    }

    #[test]
    fn descriptors_are_isolated_between_sessions() {
        let mut sessions = table();
        let alice = sessions
            .open(ALICE)
            .expect("open")
            .open(open_node(7))
            .expect("open");
        let bob = sessions
            .open(BOB)
            .expect("open")
            .open(open_node(9))
            .expect("open");
        assert_eq!(alice, bob);

        assert_eq!(session(&mut sessions, ALICE).get(alice), Ok(open_node(7)));
        assert_eq!(session(&mut sessions, BOB).get(bob), Ok(open_node(9)));

        // Закрытие у одного клиента не трогает одноимённый номер у другого.
        assert_eq!(session(&mut sessions, ALICE).close(alice), Ok(open_node(7)));
        assert_eq!(
            session(&mut sessions, ALICE).get(alice),
            Err(FileSystemError::BadDescriptor)
        );
        assert_eq!(session(&mut sessions, BOB).get(bob), Ok(open_node(9)));
    }

    #[test]
    fn unknown_and_closed_descriptors_are_rejected() {
        let mut sessions = table();
        let session = sessions.open(ALICE).expect("open");
        let fd = session.open(open_node(7)).expect("open");
        assert_eq!(
            session.get(NodeDescriptor::from_raw(fd.raw() + 1)),
            Err(FileSystemError::BadDescriptor)
        );
        assert_eq!(session.close(fd), Ok(open_node(7)));
        assert_eq!(session.close(fd), Err(FileSystemError::BadDescriptor));
    }

    #[test]
    fn closed_slot_is_reused_under_a_fresh_descriptor() {
        let mut sessions = table();
        let session = sessions.open(ALICE).expect("open");
        let first = session.open(open_node(7)).expect("open");
        let second = session.open(open_node(8)).expect("open");
        assert_eq!(session.close(first), Ok(open_node(7)));

        // Слот переиспользуется, а значение дескриптора - нет: поколение слота ушло вперёд.
        let reopened = session.open(open_node(9)).expect("open");
        assert_eq!(reopened.slot(), first.slot());
        assert_ne!(reopened, first);
        assert_eq!(session.get(second), Ok(open_node(8)));
    }

    #[test]
    fn stale_descriptor_of_a_reused_slot_is_rejected() {
        let mut sessions = table();
        let session = sessions.open(ALICE).expect("open");
        let stale = session.open(open_node(7)).expect("open");
        assert_eq!(session.close(stale), Ok(open_node(7)));
        let fresh = session.open(open_node(9)).expect("open");
        assert_eq!(fresh.slot(), stale.slot());

        // Отставший вызов закрытым дескриптором не достаёт до нового узла слота.
        assert_eq!(session.get(stale), Err(FileSystemError::BadDescriptor));
        assert_eq!(session.close(stale), Err(FileSystemError::BadDescriptor));
        assert_eq!(session.get(fresh), Ok(open_node(9)));
    }

    #[test]
    fn descriptor_limit_is_enforced_per_session() {
        let mut sessions = table();
        let session = sessions.open(ALICE).expect("open");
        let first = session.open(open_node(7)).expect("open within the limit");
        for _ in 1..OPEN_NODE_LIMIT {
            session.open(open_node(7)).expect("open within the limit");
        }
        assert_eq!(
            session.open(open_node(7)),
            Err(FileSystemError::TooManyOpenFiles)
        );

        // Потолок считает открытые дескрипторы, без учёта выданных когда-либо.
        assert_eq!(session.close(first), Ok(open_node(7)));
        let reopened = session.open(open_node(8)).expect("open");
        assert_eq!(reopened.slot(), first.slot());

        // Потолок принадлежит каждой сессии по отдельности.
        assert!(sessions.open(BOB).expect("open").open(open_node(7)).is_ok());
    }

    #[test]
    fn removed_session_forgets_its_descriptors() {
        let mut sessions = table();
        sessions
            .open(ALICE)
            .expect("open")
            .open(open_node(7))
            .expect("open");
        assert!(sessions.remove(ALICE).is_some());
        assert!(sessions.get(ALICE).is_none());
        assert!(sessions.remove(ALICE).is_none());
        // Кадр убранной сессии её не воскрешает.
        assert_eq!(
            sessions.get_mut(ALICE).err(),
            Some(FileSystemError::NoSession)
        );
    }

    #[test]
    fn attachment_is_owned_by_the_session() {
        let mut sessions: SessionTable<u32> = SessionTable::new();
        *sessions.open(ALICE).expect("open").attachment_mut() = 42;
        assert_eq!(*sessions.open(BOB).expect("open").attachment(), 0);
        assert_eq!(
            *sessions.get_mut(ALICE).expect("established").attachment(),
            42
        );
    }
}
