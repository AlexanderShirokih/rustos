use memory::memory_mapper::AddressSpaceFactory;
use process::{UserImage, UserImageError, build_user_vm_allocator, load_user_image};
use scheduler::{
    AddressSpace, ArchContext, PreparedUserProcess, PreparedUserProcessError, Priority,
    SchedulerHandle, TimerSource, UserProcessLaunch, UserProcessLaunchInfo,
};

#[derive(Debug, PartialEq, Eq)]
pub enum SpawnUserError {
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
