use runtime::OwnedHandle;

/// Сервис часов реального времени.
#[ipc::protocol(name = "Clock")]
pub trait Clock {
    /// Текущее время устройства в секундах.
    #[call]
    fn now(&self) -> u64;
}

/// Стартовые аргументы драйвера часов.
#[ipc::protocol(name = "DriverArgs")]
pub trait DriverArgs {
    /// Окно устройства и серверная часть Clock-порта.
    #[cast]
    fn provide(&self, device: ipc::wire::Cap, clock_port: ipc::wire::Cap);
}

/// Стартовые аргументы клиента часов.
#[ipc::protocol(name = "ClientArgs")]
pub trait ClientArgs {
    /// Клиентская часть Clock-порта и канал вывода в klog.
    #[cast]
    fn provide(&self, clock: ipc::wire::Cap, klog: ipc::wire::Cap);
}

/// Стартовые ресурсы драйвера, принятые по args-порту.
pub struct ClockState {
    pub device: OwnedHandle,
    pub clock_port: OwnedHandle,
}

impl DriverArgsService for Option<ClockState> {
    fn provide(&mut self, device: ipc::wire::Cap, clock_port: ipc::wire::Cap) {
        *self = Some(ClockState {
            device: OwnedHandle::adopt(device),
            clock_port: OwnedHandle::adopt(clock_port),
        });
    }
}

/// Стартовые ресурсы клиента, принятые по args-порту.
pub struct ClientState {
    pub clock: OwnedHandle,
    pub klog: OwnedHandle,
}

impl ClientArgsService for Option<ClientState> {
    fn provide(&mut self, clock: ipc::wire::Cap, klog: ipc::wire::Cap) {
        *self = Some(ClientState {
            clock: OwnedHandle::adopt(clock),
            klog: OwnedHandle::adopt(klog),
        });
    }
}
