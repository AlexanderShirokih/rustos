//! Сессия владеет управляющим клиентом, обменом и отображением зарегистрированного региона.

use core::cell::RefCell;

use filesystem_abi::{
    APPEND_OFFSET, DirCursor, IO_TRANSFER_MAX, IoPhase, IoRecord, Listed, NodeDescriptor, Written,
    file_system, fits_scratch, scratch_room,
};
use ipc::{Timeout, wire::IpcError};

use crate::{
    environment::{Environment, IoContext, SharedRegion},
    error::{Failure, Result},
};

pub(crate) struct Session<E: Environment> {
    client: file_system::Client<E::Transport>,
    io: RefCell<State<E>>,
    wait: Timeout,
}

struct State<E: Environment> {
    context: E::Io,
    binding: Option<(u64, E::Region)>,
    sequence: u64,
    failed: bool,
}

impl<E: Environment> Session<E> {
    pub(crate) fn establish(
        environment: &E,
        session: ipc::wire::Session<file_system::Protocol, E::Handle<syscall::kind::Port>>,
        wait: Timeout,
    ) -> Result<Self> {
        let transport = environment.connect(session)?;
        Ok(Self {
            client: file_system::Client::new(transport).with_wait(wait),
            io: RefCell::new(State {
                context: environment.io(),
                binding: None,
                sequence: 0,
                failed: false,
            }),
            wait,
        })
    }

    pub(crate) fn client(&self) -> &file_system::Client<E::Transport> {
        &self.client
    }

    pub(crate) fn read(&self, fd: NodeDescriptor, offset: u64, dst: &mut [u8]) -> Result<usize> {
        let request = Self::request(IoPhase::Read, fd, offset, dst.len());
        let completed = if fits_scratch(dst.len()) {
            self.staged(request, &[], dst)?
        } else {
            self.transfer(request, false, |region, start, count| {
                region.read_at(start, dst.get_mut(..count).ok_or(IpcError::InvalidValue)?)?;
                Ok(())
            })?
        };
        Ok(usize::try_from(completed.result?).map_err(|_| IpcError::InvalidValue)?)
    }

    pub(crate) fn write(&self, fd: NodeDescriptor, offset: u64, src: &[u8]) -> Result<Written> {
        let request = Self::request(IoPhase::Write, fd, offset, src.len());
        let completed = if fits_scratch(src.len()) {
            self.staged(request, src, &mut [])?
        } else {
            self.transfer(request, true, |region, start, count| {
                region.write_at(start, src.get(..count).ok_or(IpcError::InvalidValue)?)?;
                Ok(())
            })?
        };
        Ok(Written {
            count: completed.result?,
            offset: completed.offset,
        })
    }

    pub(crate) fn list(
        &self,
        fd: NodeDescriptor,
        cursor: DirCursor,
        dst: &mut [u8],
    ) -> Result<(Listed, usize)> {
        let room = scratch_room(dst.len());
        let request = Self::request(IoPhase::List, fd, u64::from(cursor.raw()), room);
        let completed = self.staged(request, &[], dst)?;
        let count = usize::try_from(completed.result?).map_err(|_| IpcError::InvalidValue)?;
        Ok((completed.listed, count))
    }

    fn request(phase: IoPhase, fd: NodeDescriptor, offset: u64, len: usize) -> IoRecord {
        let size = u32::try_from(len).unwrap_or(u32::MAX).min(IO_TRANSFER_MAX);
        IoRecord::request(0, phase, fd, offset, size)
    }

    fn transfer(
        &self,
        request: IoRecord,
        writable: bool,
        copy: impl FnOnce(&E::Region, usize, usize) -> Result<()>,
    ) -> Result<IoRecord> {
        self.operate(request, |state, request| {
            state.transfer(&self.client, request, self.wait, writable, copy)
        })
    }

    fn staged(&self, request: IoRecord, src: &[u8], dst: &mut [u8]) -> Result<IoRecord> {
        self.operate(request, |state, request| {
            staged::<E>(
                &mut state.context,
                &self.client,
                request,
                self.wait,
                src,
                dst,
            )
        })
    }

    fn operate(
        &self,
        mut request: IoRecord,
        run: impl FnOnce(&mut State<E>, IoRecord) -> Result<IoRecord>,
    ) -> Result<IoRecord> {
        let mut state = self
            .io
            .try_borrow_mut()
            .map_err(|_| IpcError::ResourceExhausted)?;
        if state.failed {
            return Err(IpcError::PeerClosed.into());
        }

        state.sequence = state
            .sequence
            .checked_add(1)
            .ok_or(IpcError::ResourceExhausted)?;
        request.id = state.sequence;

        let outcome = run(&mut state, request);

        if matches!(outcome, Err(Failure::Transport(_))) {
            state.failed = true;
            state.binding = None;
        }

        outcome
    }
}

impl<E: Environment> State<E> {
    fn transfer(
        &mut self,
        client: &file_system::Client<E::Transport>,
        request: IoRecord,
        wait: Timeout,
        writable: bool,
        copy: impl FnOnce(&E::Region, usize, usize) -> Result<()>,
    ) -> Result<IoRecord> {
        let mut ready = exchange::<E>(&mut self.context, client, request, wait)?;

        if ready.phase == IoPhase::Register {
            // Прежнее поколение освобождает свой VA до отображения нового.
            self.binding = None;

            let mapped = client
                .take_io_region(ready.region)
                .wait()
                .map_err(Failure::from)
                .and_then(|cap| self.context.map(cap, ready.writable).map_err(Failure::from));

            match mapped {
                Ok(region) => self.binding = Some((ready.region, region)),
                Err(error) => {
                    let mut abort = ready;
                    abort.phase = IoPhase::Abort;
                    exchange::<E>(&mut self.context, client, abort, wait)?;
                    return Err(error);
                }
            }

            ready.phase = IoPhase::Registered;
            ready = exchange::<E>(&mut self.context, client, ready, wait)?;
        }

        if ready.phase == IoPhase::Done {
            if ready.result? != 0 {
                return Err(IpcError::InvalidValue.into());
            }

            return Ok(ready);
        }

        if ready.phase != IoPhase::Ready {
            return Err(IpcError::InvalidValue.into());
        }

        let transfer: Result<usize> = (|| {
            let count = usize::try_from(ready.result?).map_err(|_| IpcError::InvalidValue)?;
            let start = usize::try_from(ready.start).map_err(|_| IpcError::InvalidValue)?;
            let (generation, region) = self.binding.as_ref().ok_or(IpcError::InvalidValue)?;
            if (request.offset != APPEND_OFFSET && ready.offset != request.offset)
                || ready
                    .offset
                    .checked_add(u64::try_from(count).map_err(|_| IpcError::InvalidValue)?)
                    .is_none()
                || (writable && !ready.writable)
                || *generation != ready.region
                || ready.region == 0
                || count > request.size as usize
                || start
                    .checked_add(count)
                    .is_none_or(|end| end > region.size())
            {
                return Err(IpcError::InvalidValue.into());
            }

            copy(region, start, count)?;
            Ok(count)
        })();

        let mut finish = ready;
        if let Ok(count) = transfer {
            finish.phase = IoPhase::Finish;
            finish.size = u32::try_from(count).map_err(|_| IpcError::InvalidValue)?;
        } else {
            finish.phase = IoPhase::Abort;
            finish.size = 0;
        }

        let done = exchange::<E>(&mut self.context, client, finish, wait)?;
        if done.phase != IoPhase::Done {
            return Err(IpcError::InvalidValue.into());
        }

        transfer?;
        if done.result? != finish.size
            || done.region != finish.region
            || done.offset != finish.offset
        {
            return Err(IpcError::InvalidValue.into());
        }

        Ok(done)
    }
}

/// Операция `request` через scratch сессии за один обмен: `src` ложится в scratch до запроса,
/// перенесённые байты чтения и перечисления копируются в `dst`.
pub fn staged<E: Environment>(
    context: &mut E::Io,
    client: &file_system::Client<E::Transport>,
    mut request: IoRecord,
    wait: Timeout,
    src: &[u8],
    dst: &mut [u8],
) -> Result<IoRecord> {
    request.scratch = true;
    context.scratch(client)?.write_at(0, src)?;

    let done = exchange::<E>(context, client, request, wait)?;
    if done.phase != IoPhase::Done {
        return Err(IpcError::InvalidValue.into());
    }

    let count = usize::try_from(done.result?).map_err(|_| IpcError::InvalidValue)?;
    if count > request.size as usize {
        return Err(IpcError::InvalidValue.into());
    }

    if request.phase != IoPhase::Write {
        let received = dst.get_mut(..count).ok_or(IpcError::InvalidValue)?;
        context.scratch(client)?.read_at(0, received)?;
    }

    Ok(done)
}

fn exchange<E: Environment>(
    context: &mut E::Io,
    client: &file_system::Client<E::Transport>,
    request: IoRecord,
    wait: Timeout,
) -> Result<IoRecord> {
    let response = context.exchange(client, request, wait)?;
    if response.id != request.id || response.fd != request.fd {
        return Err(IpcError::InvalidValue.into());
    }

    Ok(response)
}
