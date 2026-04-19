use drivers_common::services::scheduler::{Priority, ThreadId};

use super::{
    arch::{ArchContext, ThreadStack},
    process::ProcessId,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadState {
    Ready,
    Running,
    Blocked,
    Sleeping { wakeup_at_ns: u64 },
    Terminated,
}

pub struct Thread<A: ArchContext> {
    id: Option<ThreadId>,
    process: ProcessId,
    priority: Priority,
    state: ThreadState,
    time_slice_left: u32,
    arch: A,
    stack: ThreadStack,
    name: &'static str,
}

impl<A: ArchContext> Thread<A> {
    pub fn new(
        process: ProcessId,
        priority: Priority,
        arch: A,
        stack: ThreadStack,
        name: &'static str,
    ) -> Self {
        Self {
            id: None,
            process,
            priority,
            state: ThreadState::Ready,
            time_slice_left: 0,
            arch,
            stack,
            name,
        }
    }

    pub fn id(&self) -> ThreadId {
        self.id.expect("thread must be inserted before use")
    }

    pub(crate) fn assign_id(&mut self, id: ThreadId) {
        self.id = Some(id);
    }

    pub fn process(&self) -> ProcessId {
        self.process
    }

    pub fn priority(&self) -> Priority {
        self.priority
    }

    pub fn state(&self) -> ThreadState {
        self.state
    }

    pub fn set_state(&mut self, state: ThreadState) {
        self.state = state;
    }

    pub fn time_slice_left(&self) -> u32 {
        self.time_slice_left
    }

    pub fn set_time_slice_left(&mut self, time_slice_left: u32) {
        self.time_slice_left = time_slice_left;
    }

    pub fn arch(&self) -> &A {
        &self.arch
    }

    pub fn arch_mut(&mut self) -> &mut A {
        &mut self.arch
    }

    pub fn stack(&self) -> &ThreadStack {
        &self.stack
    }

    pub fn name(&self) -> &'static str {
        self.name
    }
}
