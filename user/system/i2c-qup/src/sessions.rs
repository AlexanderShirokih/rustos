//! Сессии `I2c` и `I2cDevice` делят порт и счётчик значков. Независимые вызовы обслуживаются
//! одновременно, а передача удерживает mutex своего контроллера до завершения или отмены.

use alloc::rc::Rc;

use component_startup::SessionPort;
use executor::Either;
use i2c_abi::{I2cError, TransferBuf, TransferError, i2c, i2c_device};
use i2c_qup::{Bus, Device, Operation};
use ipc::{Incoming, IntoWireValue, ServiceOwner, wire::IpcError};
use kernel_objects::Delivery;
use platform_abi::{DeviceId, MappedDescription};

use crate::supplies::{ControllerWindow, Interrupt, Supplies};

pub(crate) struct Service {
    pub(crate) bus: Bus<Supplies>,
    pub(crate) description: MappedDescription,
    pub(crate) sessions: Rc<SessionPort>,
}

#[derive(Clone)]
struct Held(Device<ControllerWindow, Interrupt>);

impl ServiceOwner for Held {
    type Service = Self;

    fn service(&self) -> &Self {
        self
    }
}

impl Service {
    pub(crate) fn serve(
        service: &Rc<Self>,
        frame: Incoming<'_, Delivery>,
    ) -> Result<Option<impl Future<Output = Result<(), IpcError>> + use<>>, IpcError> {
        let description = service
            .description
            .description()
            .map_err(|_| IpcError::InvalidValue)?;

        let call = if let Some(device) = service.bus.device(&description, frame.resources.badge) {
            i2c_device::serve(&Held(device), frame)?.map(Either::Left)
        } else {
            i2c::serve(service, frame)?.map(Either::Right)
        };

        Ok(call.map(Either::run))
    }
}

impl i2c::Service<Delivery> for Service {
    async fn open(
        &self,
        _frame: &Delivery,
        device: DeviceId,
    ) -> impl IntoWireValue<Delivery, Read = Result<ipc::wire::Session<i2c_device::Protocol>, I2cError>>
    {
        let description = self
            .description
            .description()
            .map_err(|_| I2cError::NoDevice)?;
        self.bus.prepare(&description, &device).await?;

        let minted = self
            .sessions
            .create_session()
            .map_err(|_| I2cError::NoSpace)?;
        self.bus.claim(minted.badge.get(), device);

        Ok(kernel_objects::Session::from_handle(minted.port))
    }
}

impl i2c_device::Service<Delivery> for Held {
    async fn write(&self, _frame: &Delivery, data: &[u8]) -> Result<(), TransferError> {
        self.0.transfer(Operation::Write(data)).await.map(drop)
    }

    async fn read(&self, _frame: &Delivery, count: u16) -> Result<TransferBuf, TransferError> {
        self.0.transfer(Operation::Read(count)).await
    }

    async fn write_read(
        &self,
        _frame: &Delivery,
        data: &[u8],
        count: u16,
    ) -> Result<TransferBuf, TransferError> {
        self.0.transfer(Operation::WriteRead(data, count)).await
    }
}
