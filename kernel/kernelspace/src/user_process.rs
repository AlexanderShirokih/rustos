use memory::memory_mapper::AddressSpaceFactory;
use process::{UserImage, UserImageError, build_user_vm_allocator, load_user_image};
use scheduler::{
    AddressSpace, ArchContext, Bootstrapped, PreparedUserProcess, PreparedUserProcessError,
    Priority, Running, Scheduler, SchedulerHandle, ThreadId, TimerSource, UserProcessLaunch,
    UserProcessLaunchInfo,
};

#[derive(Debug, PartialEq, Eq)]
pub enum SpawnUserError {
    MissingFactory,
    Image(UserImageError),
    Prepared(PreparedUserProcessError),
}

impl From<UserImageError> for SpawnUserError {
    fn from(value: UserImageError) -> Self {
        Self::Image(value)
    }
}

impl From<PreparedUserProcessError> for SpawnUserError {
    fn from(value: PreparedUserProcessError) -> Self {
        Self::Prepared(value)
    }
}

pub trait UserProcessSpawner<A, T, S>
where
    A: ArchContext,
    T: TimerSource,
{
    fn spawn_user_process(
        &self,
        name: &'static str,
        image: &UserImage<'_>,
        priority: Priority,
        kernel_stack_pages: usize,
    ) -> Result<(scheduler::ProcessId, ThreadId), SpawnUserError> {
        let info = self.spawn_user_process_with_launch(
            name,
            image,
            priority,
            kernel_stack_pages,
            UserProcessLaunch::default(),
        )?;
        Ok((info.process_id, info.thread_id))
    }

    fn spawn_user_process_with_launch(
        &self,
        name: &'static str,
        image: &UserImage<'_>,
        priority: Priority,
        kernel_stack_pages: usize,
        launch: UserProcessLaunch,
    ) -> Result<UserProcessLaunchInfo, SpawnUserError>;
}

pub trait UserProcessLauncher: Send + Sync {
    fn spawn_user_process_with_launch(
        &self,
        name: &'static str,
        image: &UserImage<'_>,
        priority: Priority,
        kernel_stack_pages: usize,
        launch: UserProcessLaunch,
    ) -> Result<UserProcessLaunchInfo, SpawnUserError>;
}

pub struct SchedulerUserProcessLauncher<A, T>
where
    A: ArchContext,
    T: TimerSource,
{
    handle: SchedulerHandle<A, T>,
    factory: &'static (dyn AddressSpaceFactory + Send + Sync),
}

impl<A, T> SchedulerUserProcessLauncher<A, T>
where
    A: ArchContext,
    T: TimerSource,
{
    pub fn new(
        handle: SchedulerHandle<A, T>,
        factory: &'static (dyn AddressSpaceFactory + Send + Sync),
    ) -> Self {
        Self { handle, factory }
    }
}

impl<A, T> UserProcessLauncher for SchedulerUserProcessLauncher<A, T>
where
    A: ArchContext,
    T: TimerSource,
{
    fn spawn_user_process_with_launch(
        &self,
        name: &'static str,
        image: &UserImage<'_>,
        priority: Priority,
        kernel_stack_pages: usize,
        launch: UserProcessLaunch,
    ) -> Result<UserProcessLaunchInfo, SpawnUserError> {
        spawn_user_process_with_factory(
            name,
            image,
            priority,
            kernel_stack_pages,
            launch,
            self.factory,
            |prepared| self.handle.spawn_prepared_user_process(prepared),
        )
    }
}

impl<A, T> UserProcessSpawner<A, T, Bootstrapped> for Scheduler<A, T, Bootstrapped>
where
    A: ArchContext,
    T: TimerSource,
{
    fn spawn_user_process_with_launch(
        &self,
        name: &'static str,
        image: &UserImage<'_>,
        priority: Priority,
        kernel_stack_pages: usize,
        launch: UserProcessLaunch,
    ) -> Result<UserProcessLaunchInfo, SpawnUserError> {
        let factory = self
            .address_space_factory()
            .ok_or(SpawnUserError::MissingFactory)?;
        spawn_user_process_with_factory(
            name,
            image,
            priority,
            kernel_stack_pages,
            launch,
            factory,
            |prepared| self.spawn_prepared_user_process(prepared),
        )
    }
}

impl<A, T> UserProcessSpawner<A, T, Running> for Scheduler<A, T, Running>
where
    A: ArchContext,
    T: TimerSource,
{
    fn spawn_user_process_with_launch(
        &self,
        name: &'static str,
        image: &UserImage<'_>,
        priority: Priority,
        kernel_stack_pages: usize,
        launch: UserProcessLaunch,
    ) -> Result<UserProcessLaunchInfo, SpawnUserError> {
        let factory = self
            .address_space_factory()
            .ok_or(SpawnUserError::MissingFactory)?;
        spawn_user_process_with_factory(
            name,
            image,
            priority,
            kernel_stack_pages,
            launch,
            factory,
            |prepared| self.spawn_prepared_user_process(prepared),
        )
    }
}

fn spawn_user_process_with_factory<F>(
    name: &'static str,
    image: &UserImage<'_>,
    priority: Priority,
    kernel_stack_pages: usize,
    launch: UserProcessLaunch,
    factory: &'static (dyn AddressSpaceFactory + Send + Sync),
    spawn_prepared: F,
) -> Result<UserProcessLaunchInfo, SpawnUserError>
where
    F: FnOnce(PreparedUserProcess) -> Result<UserProcessLaunchInfo, PreparedUserProcessError>,
{
    let prepared =
        prepare_user_process(name, image, priority, kernel_stack_pages, launch, factory)?;
    spawn_prepared(prepared).map_err(Into::into)
}

fn prepare_user_process(
    name: &'static str,
    image: &UserImage<'_>,
    priority: Priority,
    kernel_stack_pages: usize,
    launch: UserProcessLaunch,
    factory: &'static (dyn AddressSpaceFactory + Send + Sync),
) -> Result<PreparedUserProcess, SpawnUserError> {
    if let Some(index) = launch.bootstrap_handle_index
        && index >= launch.initial_handles.len()
    {
        return Err(PreparedUserProcessError::InvalidBootstrapHandle.into());
    }
    if launch.initial_handles.len() > capability::HandleTable::DEFAULT_CAPACITY as usize {
        return Err(PreparedUserProcessError::TooManyInitialHandles.into());
    }

    image.validate()?;

    let address_space = AddressSpace::new_user(factory).map_err(|_| {
        PreparedUserProcessError::Spawn(scheduler::SpawnError::AddressSpaceCreationFailed)
    })?;

    let mapper = address_space
        .mapper()
        .expect("AddressSpace::User must expose mapper");

    load_user_image(mapper, image)?;

    Ok(PreparedUserProcess {
        name,
        priority,
        kernel_stack_pages,
        address_space,
        user_pc: image.entry,
        user_sp: image.user_stack_top,
        user_vm: build_user_vm_allocator(image),
        launch,
    })
}
