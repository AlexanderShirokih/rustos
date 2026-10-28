//! Задача активной операции сессии удерживает окно до Finish/Abort или срока.

use alloc::{
    rc::{Rc, Weak},
    vec::Vec,
};
use core::cell::RefCell;

use executor::{Mutex, TaskHandle};
use filesystem::{ClientId, NodeId, Window};
use filesystem_abi::{
    APPEND_OFFSET, AccessMode, DirCursor, FileSystemError, IO_FRAME_BYTES, IO_SCRATCH_BYTES,
    IO_TRANSFER_MAX, IoBootstrap, IoPhase, IoRecord, NodeDescriptor,
};
use ipc::{
    Timeout,
    wire::{IpcError, MessageBuf},
};
use kernel_objects::{Mapping, MemoryRegion, RingConsumer, RingProducer, Signal};
use runtime::{MemoryAccess, UserMemFlags};
use syscall::Rights;
use userland::USERLAND_PAGE_SIZE;

use crate::{error::no_space, server::Shared, service::FileSystemState, store::window_rights};

const OPERATION_TIMEOUT: Timeout = Timeout::from_secs(2);
const RING_BYTES: u64 = USERLAND_PAGE_SIZE;
const OPERATIONS_PER_TURN: usize = 16;

pub(crate) struct Lease {
    shared: Rc<Shared>,
    volume: usize,
    pub(crate) node: NodeId,
    pub(crate) lock: Rc<Mutex<()>>,
}

pub(crate) struct Exchange {
    registration: Rc<RefCell<Registration>>,
    scratch: MemoryRegion,
    _task: TaskHandle,
}

#[derive(Default)]
struct Registration {
    binding: Option<Binding>,
    generation: u64,
}

struct Binding {
    window: Option<WindowKey>,
    writable: bool,
    region: MemoryRegion,
    stage: Stage,
    _node_lock: Rc<Mutex<()>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct WindowKey {
    generation: u64,
    offset: u64,
    size: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Offered,
    Claimed,
    Registered,
}

struct Worker {
    shared: Rc<Shared>,
    volume: usize,
    client: ClientId,
    requests: RingConsumer,
    responses: RingProducer,
    registration: Rc<RefCell<Registration>>,
    outgoing: RefCell<MessageBuf>,
    incoming: RefCell<[u8; IO_FRAME_BYTES]>,
    scratch: Mapping,
    listing: RefCell<Vec<u8>>,
    node_lock: RefCell<Option<Rc<Mutex<()>>>>,
}

struct Prepared {
    ready: IoRecord,
    staging_window: Option<MemoryRegion>,
    target: Option<Window>,
    writing: bool,
}

impl Lease {
    pub(crate) fn node(
        shared: Rc<Shared>,
        volume: usize,
        node: NodeId,
    ) -> Result<Self, FileSystemError> {
        Self::found(shared, volume, |_| Ok(node))
    }

    pub(crate) fn descriptor(
        shared: Rc<Shared>,
        volume: usize,
        client: ClientId,
        fd: NodeDescriptor,
    ) -> Result<Self, FileSystemError> {
        Self::found(shared, volume, |state| {
            Ok(state.sessions.get_mut(client)?.get(fd)?.node)
        })
    }

    fn found(
        shared: Rc<Shared>,
        volume: usize,
        find: impl FnOnce(&mut FileSystemState) -> Result<NodeId, FileSystemError>,
    ) -> Result<Self, FileSystemError> {
        let (node, lock) = shared.with_state(volume, |state| {
            let node = find(state)?;
            state.backend.retain(node)?;

            if let Some(lock) = state.locks.get(&node).and_then(Weak::upgrade) {
                return Ok((node, lock));
            }

            state.locks.retain(|_, lock| lock.strong_count() != 0);
            let lock = Rc::new(Mutex::new(()));
            state.locks.insert(node, Rc::downgrade(&lock));
            Ok((node, lock))
        })?;

        Ok(Self {
            shared,
            volume,
            node,
            lock,
        })
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let _ = self.shared.with_state(self.volume, |state| {
            state.backend.release(&mut state.stores, self.node);
            Ok(())
        });
    }
}

impl Exchange {
    pub(crate) fn start(
        shared: &Rc<Shared>,
        volume: usize,
        client: ClientId,
    ) -> Result<IoBootstrap<MemoryRegion, Signal>, FileSystemError> {
        shared.with_state(volume, |state| {
            if state.sessions.get_mut(client)?.attachment().io.is_some() {
                return Err(FileSystemError::AlreadyExists);
            }

            Ok(())
        })?;

        let executor = shared.executor.upgrade().ok_or(FileSystemError::NoSpace)?;
        let resource = kernel_objects::Resource::self_resource().map_err(no_space)?;
        let requests = resource
            .create_virtual(RING_BYTES, MemoryAccess::RW)
            .map_err(no_space)?;
        let responses = resource
            .create_virtual(RING_BYTES, MemoryAccess::RW)
            .map_err(no_space)?;
        let scratch = resource
            .create_virtual(u64::from(IO_SCRATCH_BYTES), MemoryAccess::RW)
            .map_err(no_space)?;
        let request_ready = Signal::create().map_err(no_space)?;
        let response_ready = Signal::create().map_err(no_space)?;

        let rights = Rights::READ | Rights::WRITE | Rights::TRANSFER;
        let ends = IoBootstrap {
            requests: requests.duplicate(rights, None).map_err(no_space)?,
            request_ready: request_ready.duplicate(rights, None).map_err(no_space)?,
            responses: responses.duplicate(rights, None).map_err(no_space)?,
            response_ready: response_ready.duplicate(rights, None).map_err(no_space)?,
        };
        let root = scratch
            .duplicate(window_rights(true, true), None)
            .map_err(no_space)?;

        let registration = Rc::new(RefCell::new(Registration::default()));
        let worker = Worker {
            shared: shared.clone(),
            volume,
            client,
            requests: RingConsumer::new(requests, IO_FRAME_BYTES, request_ready)
                .ok_or(FileSystemError::NoSpace)?,
            responses: RingProducer::new(responses, IO_FRAME_BYTES, response_ready)
                .ok_or(FileSystemError::NoSpace)?,
            registration: registration.clone(),
            outgoing: RefCell::new(MessageBuf::new()),
            incoming: RefCell::new([0; IO_FRAME_BYTES]),
            scratch: Mapping::full(scratch, UserMemFlags::ReadWrite)
                .map_err(|refused| no_space(refused.error))?,
            listing: RefCell::new(Vec::new()),
            node_lock: RefCell::new(None),
        };

        let task = executor
            .spawn_now(async move {
                let _ = worker.run().await;
            })
            .abort_on_drop();

        shared.with_state(volume, |state| {
            state.sessions.get_mut(client)?.attachment_mut().io = Some(Self {
                registration,
                scratch: root,
                _task: task,
            });
            Ok(())
        })?;

        Ok(ends)
    }

    pub(crate) fn scratch(&self) -> Result<MemoryRegion, FileSystemError> {
        self.scratch
            .duplicate(window_rights(true, false), None)
            .map_err(no_space)
    }

    pub(crate) fn claim(&self, generation: u64) -> Result<MemoryRegion, FileSystemError> {
        let mut registration = self.registration.borrow_mut();
        let current = registration.generation == generation;
        let binding = registration
            .binding
            .as_mut()
            .filter(|binding| current && binding.stage == Stage::Offered)
            .ok_or(FileSystemError::NoBuffer)?;
        let region = binding
            .region
            .duplicate(window_rights(binding.writable, false), None)
            .map_err(no_space)?;

        binding.stage = Stage::Claimed;
        Ok(region)
    }
}

impl Worker {
    async fn receive(&self) -> Result<IoRecord, IpcError> {
        loop {
            {
                let mut bytes = self.incoming.borrow_mut();
                if let Some(len) = self.requests.try_pop(&mut *bytes)? {
                    return IoRecord::decode(bytes.get(..len).ok_or(IpcError::Truncated)?);
                }
            }

            executor::wait(self.requests.wait_item())
                .await
                .map_err(|_| IpcError::PeerClosed)?;
        }
    }

    fn send(&self, record: IoRecord) -> Result<(), IpcError> {
        let mut outgoing = self.outgoing.borrow_mut();
        record.encode(&mut outgoing)?;
        self.responses.push(outgoing.as_bytes())
    }

    async fn run(self) -> Result<(), IpcError> {
        let mut sequence = 0;
        let mut served = 0;
        loop {
            let request = self.receive().await?;
            if request.id <= sequence
                || !matches!(
                    request.phase,
                    IoPhase::Read | IoPhase::Write | IoPhase::List
                )
            {
                return Err(IpcError::InvalidValue);
            }
            sequence = request.id;
            let mut done = request;
            done.phase = IoPhase::Done;
            match self.operation(request).await {
                Ok(completed) => self.send(completed)?,
                Err(error) => {
                    done.result = Err(error);
                    self.send(done)?;
                }
            }
            served += 1;
            if served == OPERATIONS_PER_TURN {
                served = 0;
                executor::yield_now().await;
            }
        }
    }

    async fn operation(&self, request: IoRecord) -> Result<IoRecord, FileSystemError> {
        if request.size > IO_TRANSFER_MAX {
            return Err(FileSystemError::BufferTooSmall);
        }

        let fd = NodeDescriptor::from_raw(request.fd);
        let mut opened = None;
        let lease = Lease::found(self.shared.clone(), self.volume, |state| {
            let node = state.sessions.get_mut(self.client)?.get(fd)?;
            opened = Some(node);
            Ok(node.node)
        })?;
        let opened = opened.ok_or(FileSystemError::NoSession)?;

        // Worker хранит замок последнего узла сессии, и следующая операция над этим узлом
        // берёт тот же замок.
        let _ = self.node_lock.replace(Some(lease.lock.clone()));
        let _locked = lease.lock.lock().await;

        if (request.phase == IoPhase::List || request.scratch) && request.size > IO_SCRATCH_BYTES {
            return Err(FileSystemError::BufferTooSmall);
        }

        if request.phase == IoPhase::List {
            return self.list(opened, request);
        }

        if request.scratch {
            return self.shared.with_state(self.volume, |state| {
                let prepared = prepare(state, opened, request)?;
                self.through_scratch(state, lease.node, &prepared)
            });
        }

        let mut prepared = self
            .shared
            .with_state(self.volume, |state| prepare(state, opened, request))?;
        if prepared.ready.result == Ok(0) {
            prepared.ready.phase = IoPhase::Done;
            return Ok(prepared.ready);
        }

        let window = self.shared.with_state(self.volume, |state| {
            prepare_window(state, opened.mode, &mut prepared)
        })?;
        let writable = prepared.ready.writable;

        {
            let mut registration = self.registration.borrow_mut();
            if registration.binding.as_ref().is_none_or(|binding| {
                window.is_none() || binding.window != window || binding.writable != writable
            }) {
                let mode = if writable {
                    AccessMode::ReadWrite
                } else {
                    AccessMode::Read
                };

                let root = if let Some(region) = &prepared.staging_window {
                    region
                        .duplicate(window_rights(writable, true), None)
                        .map_err(no_space)?
                } else {
                    self.shared.with_state(self.volume, |state| {
                        state.stores.slice(
                            prepared.target.ok_or(FileSystemError::NotSupported)?,
                            mode,
                            true,
                        )
                    })?
                };

                registration.generation = registration
                    .generation
                    .checked_add(1)
                    .ok_or(FileSystemError::NoSpace)?;
                registration.binding = Some(Binding {
                    window,
                    writable,
                    region: root,
                    stage: Stage::Offered,
                    _node_lock: lease.lock.clone(),
                });
            }

            let binding = registration
                .binding
                .as_ref()
                .ok_or(FileSystemError::NoBuffer)?;
            prepared.ready.region = registration.generation;
            prepared.ready.phase = if binding.stage == Stage::Registered {
                IoPhase::Ready
            } else {
                IoPhase::Register
            };
        }

        // Один срок ограничивает регистрацию и перенос. Отмена future освобождает блокировку узла.
        let completed = executor::timeout(OPERATION_TIMEOUT, self.complete(&lease, prepared)).await;
        if !matches!(completed, Some(Ok(_))) {
            self.registration.borrow_mut().binding = None;
        }

        completed.ok_or(FileSystemError::Unresponsive)?
    }

    async fn complete(
        &self,
        lease: &Lease,
        prepared: Prepared,
    ) -> Result<IoRecord, FileSystemError> {
        let mut ready = prepared.ready;
        self.send(ready)
            .map_err(|_| FileSystemError::Unresponsive)?;

        let mut next = self
            .receive()
            .await
            .map_err(|_| FileSystemError::Unresponsive)?;

        if ready.phase == IoPhase::Register && next.phase != IoPhase::Abort {
            Self::validate(ready, next)?;
            if next.phase != IoPhase::Registered {
                return Err(FileSystemError::NotSupported);
            }
            {
                let mut registration = self.registration.borrow_mut();
                let binding = registration
                    .binding
                    .as_mut()
                    .ok_or(FileSystemError::NoBuffer)?;
                if binding.stage == Stage::Offered {
                    return Err(FileSystemError::NoBuffer);
                }

                binding.stage = Stage::Registered;
            }

            ready.phase = IoPhase::Ready;
            self.send(ready)
                .map_err(|_| FileSystemError::Unresponsive)?;
            next = self
                .receive()
                .await
                .map_err(|_| FileSystemError::Unresponsive)?;
        }

        Self::validate(ready, next)?;

        let mut done = ready;
        done.phase = IoPhase::Done;
        if next.phase == IoPhase::Abort {
            if self
                .registration
                .borrow()
                .binding
                .as_ref()
                .is_some_and(|binding| binding.stage != Stage::Registered)
            {
                self.registration.borrow_mut().binding = None;
            }

            done.result = Ok(0);
            return Ok(done);
        }

        if next.phase != IoPhase::Finish || next.size > ready.result? {
            return Err(FileSystemError::BufferTooSmall);
        }

        if prepared.writing && next.size != 0 {
            self.shared.with_state(self.volume, |state| {
                if let Some(source) = &prepared.staging_window {
                    let target = prepared.target.ok_or(FileSystemError::NotSupported)?;
                    state
                        .stores
                        .copy_from(target, source, ready.offset, next.size as usize)?;
                }

                commit_write(state, lease.node, ready.offset, next.size)
            })?;
        }

        done.result = Ok(next.size);

        Ok(done)
    }

    fn through_scratch(
        &self,
        state: &mut FileSystemState,
        node: NodeId,
        prepared: &Prepared,
    ) -> Result<IoRecord, FileSystemError> {
        let mut done = prepared.ready;
        done.phase = IoPhase::Done;

        let count = done.result?;
        if count == 0 {
            return Ok(done);
        }

        let target = prepared.target.ok_or(FileSystemError::NotSupported)?;
        if prepared.writing {
            state
                .stores
                .copy_from_pages(target, done.offset, &self.scratch, count as usize)?;
            commit_write(state, node, done.offset, count)?;
        } else {
            state
                .stores
                .copy_into_pages(target, done.offset, &self.scratch, count as usize)?;
        }

        Ok(done)
    }

    fn list(
        &self,
        opened: filesystem::OpenNode,
        request: IoRecord,
    ) -> Result<IoRecord, FileSystemError> {
        if !opened.mode.readable() {
            return Err(FileSystemError::WrongMode);
        }

        let size = request.size as usize;
        let cursor = DirCursor::from_raw(
            u32::try_from(request.offset).map_err(|_| FileSystemError::NotSupported)?,
        );
        let mut listing = self.listing.borrow_mut();
        let missing = size.saturating_sub(listing.len());
        if missing != 0 {
            listing
                .try_reserve_exact(missing)
                .map_err(|_| FileSystemError::NoSpace)?;
            listing.resize(size, 0);
        }

        let bytes = listing
            .get_mut(..size)
            .ok_or(FileSystemError::BufferTooSmall)?;
        let batch = self.shared.with_state(self.volume, |state| {
            state.backend.list(opened.node, cursor, bytes)
        })?;

        self.scratch
            .copy_in_at(
                0,
                bytes
                    .get(..batch.len)
                    .ok_or(FileSystemError::BufferTooSmall)?,
            )
            .map_err(no_space)?;

        let mut done = request;
        done.phase = IoPhase::Done;
        done.listed = batch.listed;
        done.writable = false;
        done.result = Ok(u32::try_from(batch.len).map_err(|_| FileSystemError::BufferTooSmall)?);
        Ok(done)
    }

    fn validate(ready: IoRecord, next: IoRecord) -> Result<(), FileSystemError> {
        if next.id != ready.id || next.fd != ready.fd || next.region != ready.region {
            return Err(FileSystemError::NotSupported);
        }

        Ok(())
    }
}

fn prepare(
    state: &mut FileSystemState,
    opened: filesystem::OpenNode,
    mut ready: IoRecord,
) -> Result<Prepared, FileSystemError> {
    let writing = ready.phase == IoPhase::Write;
    if (writing && !opened.mode.writable()) || (!writing && !opened.mode.readable()) {
        return Err(FileSystemError::WrongMode);
    }

    let metadata = state.backend.metadata(opened.node)?;

    let mut prepared = Prepared {
        ready,
        staging_window: None,
        target: None,
        writing,
    };

    if metadata.kind.is_directory() {
        return Err(FileSystemError::IsADirectory);
    }

    if writing && ready.offset == APPEND_OFFSET {
        ready.offset = metadata.size;
    }

    let count = if writing {
        ready.size
    } else {
        u32::try_from(
            metadata
                .size
                .saturating_sub(ready.offset)
                .min(u64::from(ready.size)),
        )
        .map_err(|_| FileSystemError::NoSpace)?
    };

    ready.result = Ok(count);
    prepared.ready = ready;
    if count == 0 {
        return Ok(prepared);
    }

    let window = if writing {
        state
            .backend
            .prepare_write(&mut state.stores, opened.node, ready.offset, count as usize)?
    } else {
        state.backend.read_window(opened.node)?
    };

    prepared.target = Some(window);
    Ok(prepared)
}

fn prepare_window(
    state: &FileSystemState,
    mode: AccessMode,
    prepared: &mut Prepared,
) -> Result<Option<WindowKey>, FileSystemError> {
    prepared.ready.writable = mode.writable();
    if prepared.writing && !mode.readable() {
        prepared.staging_window = Some(fresh(state, prepared.ready.result? as usize)?);
        prepared.ready.start = 0;
        return Ok(None);
    }

    prepared.ready.start = prepared.ready.offset;
    prepared
        .target
        .map(|window| {
            Ok(WindowKey {
                generation: state.stores.generation(window.store)?,
                offset: window.offset,
                size: window.size,
            })
        })
        .transpose()
}

fn commit_write(
    state: &mut FileSystemState,
    node: NodeId,
    offset: u64,
    count: u32,
) -> Result<(), FileSystemError> {
    let end = offset
        .checked_add(u64::from(count))
        .ok_or(FileSystemError::NoSpace)?;
    state.backend.commit_write(node, end)
}

fn fresh(state: &FileSystemState, size: usize) -> Result<MemoryRegion, FileSystemError> {
    let size = u64::try_from(size)
        .map_err(|_| FileSystemError::NoSpace)?
        .div_ceil(USERLAND_PAGE_SIZE)
        * USERLAND_PAGE_SIZE;
    state
        .stores
        .resource()
        .create_virtual(size, MemoryAccess::RW)
        .map_err(no_space)
}
