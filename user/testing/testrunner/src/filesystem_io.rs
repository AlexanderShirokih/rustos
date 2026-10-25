//! Прямой участник протокола для проверок дескрипторов и недоверенных записей.

use core::{cell::RefCell, ops::Deref};

use filesystem_abi::{
    DirCursor, FileSystemError, IoPhase, IoRecord, Listed, NodeDescriptor, Written, file_system,
    fits_scratch, scratch_room,
};
use filesystem_client::{
    Environment, Failure, FileSystemEnvironment, IoContext, SessionIo, SessionRegion, SharedRegion,
};
use ipc::{CallError, Timeout, wire::IpcError};
use kernel_objects::SessionTransport;

pub(crate) struct IoClient {
    control: file_system::Client<SessionTransport>,
    state: RefCell<State>,
}

struct State {
    context: SessionIo,
    binding: Option<(u64, SessionRegion)>,
    sequence: u64,
}

impl Deref for IoClient {
    type Target = file_system::Client<SessionTransport>;
    fn deref(&self) -> &Self::Target {
        &self.control
    }
}

impl IoClient {
    pub(crate) fn new(control: file_system::Client<SessionTransport>) -> Self {
        Self {
            control,
            state: RefCell::new(State {
                context: FileSystemEnvironment.io(),
                binding: None,
                sequence: 0,
            }),
        }
    }

    pub(crate) fn into_channel(self) -> SessionTransport {
        self.control.into_channel()
    }

    pub(crate) fn read(
        &self,
        fd: NodeDescriptor,
        offset: u64,
        dst: &mut [u8],
    ) -> Result<u32, CallError<FileSystemError>> {
        if fits_scratch(dst.len()) {
            return self
                .staged(IoPhase::Read, fd, offset, dst.len(), &[], dst)
                .map(|done| done.result)
                .and_then(|result| result.map_err(CallError::Remote));
        }

        self.with_window(
            IoPhase::Read,
            fd,
            offset,
            dst.len(),
            |region, start, count| {
                region.read_at(
                    start,
                    dst.get_mut(..count)
                        .ok_or(FileSystemError::BufferTooSmall)?,
                )?;
                u32::try_from(count).map_err(|_| FileSystemError::BufferTooSmall)
            },
        )
        .map(|done| done.result)
        .and_then(|result| result.map_err(CallError::Remote))
    }

    pub(crate) fn write(
        &self,
        fd: NodeDescriptor,
        offset: u64,
        src: &[u8],
    ) -> Result<Written, CallError<FileSystemError>> {
        if fits_scratch(src.len()) {
            let done = self.staged(IoPhase::Write, fd, offset, src.len(), src, &mut [])?;
            return Ok(Written {
                count: done.result.map_err(CallError::Remote)?,
                offset: done.offset,
            });
        }

        let done = self.with_window(
            IoPhase::Write,
            fd,
            offset,
            src.len(),
            |region, start, count| {
                region.write_at(
                    start,
                    src.get(..count).ok_or(FileSystemError::BufferTooSmall)?,
                )?;
                u32::try_from(count).map_err(|_| FileSystemError::BufferTooSmall)
            },
        )?;
        Ok(Written {
            count: done.result.map_err(CallError::Remote)?,
            offset: done.offset,
        })
    }

    pub(crate) fn list(
        &self,
        fd: NodeDescriptor,
        cursor: DirCursor,
        dst: &mut [u8],
    ) -> Result<Listed, CallError<FileSystemError>> {
        let room = scratch_room(dst.len());
        self.staged(IoPhase::List, fd, u64::from(cursor.raw()), room, &[], dst)
            .map(|done| done.listed)
    }

    fn staged(
        &self,
        phase: IoPhase,
        fd: NodeDescriptor,
        offset: u64,
        len: usize,
        src: &[u8],
        dst: &mut [u8],
    ) -> Result<IoRecord, CallError<FileSystemError>> {
        let mut state = self.state.borrow_mut();
        state.sequence += 1;
        let size =
            u32::try_from(len).map_err(|_| CallError::Remote(FileSystemError::BufferTooSmall))?;
        let request = IoRecord::request(state.sequence, phase, fd, offset, size);

        filesystem_client::staged::<FileSystemEnvironment>(
            &mut state.context,
            &self.control,
            request,
            Timeout::from_secs(2),
            src,
            dst,
        )
        .map_err(call_error)
    }

    pub(crate) fn with_window(
        &self,
        phase: IoPhase,
        fd: NodeDescriptor,
        offset: u64,
        len: usize,
        copy: impl FnOnce(&SessionRegion, usize, usize) -> Result<u32, FileSystemError>,
    ) -> Result<IoRecord, CallError<FileSystemError>> {
        let mut state = self.state.borrow_mut();
        state.sequence += 1;
        let size =
            u32::try_from(len).map_err(|_| CallError::Remote(FileSystemError::BufferTooSmall))?;
        let request = IoRecord::request(state.sequence, phase, fd, offset, size);

        let wait = Timeout::from_secs(2);
        let mut ready = state
            .context
            .exchange(&self.control, request, wait)
            .map_err(call_error)?;
        ready.result.map_err(CallError::Remote)?;
        if ready.phase == IoPhase::Done {
            return Ok(ready);
        }

        if ready.phase == IoPhase::Register {
            // Прежнее поколение освобождает свой VA до отображения нового.
            state.binding = None;
            let cap = self.control.take_io_region(ready.region).wait()?;
            let mapped = state.context.map(cap, ready.writable);
            match mapped {
                Ok(region) => state.binding = Some((ready.region, region)),
                Err(error) => {
                    ready.phase = IoPhase::Abort;
                    state
                        .context
                        .exchange(&self.control, ready, wait)
                        .map_err(call_error)?;
                    return Err(CallError::Remote(error));
                }
            }

            ready.phase = IoPhase::Registered;
            ready = state
                .context
                .exchange(&self.control, ready, wait)
                .map_err(call_error)?;
        }

        let (generation, region) = state
            .binding
            .as_ref()
            .ok_or(CallError::Ipc(IpcError::InvalidValue))?;
        if ready.phase != IoPhase::Ready || ready.id != request.id || *generation != ready.region {
            return Err(CallError::Ipc(IpcError::InvalidValue));
        }

        let count = ready.result.map_err(CallError::Remote)?;
        if count > request.size {
            return Err(CallError::Ipc(IpcError::InvalidValue));
        }

        let copied = copy(
            region,
            usize::try_from(ready.start).map_err(|_| CallError::Ipc(IpcError::InvalidValue))?,
            count as usize,
        );
        ready.phase = if copied.is_ok() {
            IoPhase::Finish
        } else {
            IoPhase::Abort
        };

        ready.size = copied.as_ref().copied().unwrap_or(0);
        let done = state
            .context
            .exchange(&self.control, ready, wait)
            .map_err(call_error)?;
        copied.map_err(CallError::Remote)?;
        done.result.map_err(CallError::Remote)?;
        Ok(done)
    }
}

fn call_error(error: Failure) -> CallError<FileSystemError> {
    match error {
        Failure::FileSystem(error) => CallError::Remote(error),
        Failure::Transport(error) => CallError::Ipc(error),
    }
}
