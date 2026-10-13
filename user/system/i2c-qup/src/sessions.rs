//! Сессии `I2c` и сессии устройств выпускаются на одном порту из общего счётчика значков.
//! Кадр значка, которым занято устройство, обслуживает протокол `I2cDevice`, остальные кадры -
//! протокол `I2c`. Передача держит цикл сессий до ответа, и передачи контроллера идут по
//! одной.

use component_startup::{SessionEvent, SessionPort};
use i2c_abi::{I2cError, TransferBuf, TransferError, i2c, i2c_device};
use i2c_qup::{Bus, Device, Operation};
use ipc::IntoWireValue;
use kernel_objects::{Delivery, IrqLine, MmioMapping};
use platform_abi::DeviceId;

use crate::supplies::Supplies;

struct Opener<'s, 'p, 'd> {
    bus: &'s mut Bus<'d, Supplies<'p>>,
    sessions: &'s SessionPort,
}

struct Held<'t>(Device<'t, MmioMapping, IrqLine>);

impl i2c::Service<Delivery> for Opener<'_, '_, '_> {
    fn open(
        &mut self,
        _frame: &Delivery,
        device: DeviceId,
    ) -> impl IntoWireValue<Delivery, Read = Result<ipc::wire::Session<i2c_device::Protocol>, I2cError>>
    {
        self.bus.prepare(&device)?;
        let minted = self
            .sessions
            .create_session()
            .map_err(|_| I2cError::NoSpace)?;
        self.bus.claim(minted.badge.get(), device);

        // Порт сессии устройства выпускает эта шина, и она же обслуживает на нём `I2cDevice`.
        Ok(kernel_objects::Session::from_handle(minted.port))
    }
}

impl i2c_device::Service<Delivery> for Held<'_> {
    fn write(&mut self, _frame: &Delivery, data: &[u8]) -> Result<(), TransferError> {
        self.0.transfer(Operation::Write(data)).map(drop)
    }

    fn read(&mut self, _frame: &Delivery, count: u16) -> Result<TransferBuf, TransferError> {
        self.0.transfer(Operation::Read(count))
    }

    fn write_read(
        &mut self,
        _frame: &Delivery,
        data: &[u8],
        count: u16,
    ) -> Result<TransferBuf, TransferError> {
        self.0.transfer(Operation::WriteRead(data, count))
    }
}

pub(crate) fn serve(bus: &mut Bus<'_, Supplies<'_>>, sessions: &SessionPort) {
    let mut events = sessions.events();

    while let Some(event) = events.recv() {
        match event {
            SessionEvent::Frame(frame) => {
                if let Some(device) = bus.device(frame.resources.badge) {
                    let _ = i2c_device::dispatch(&mut Held(device), frame);
                } else {
                    let _ = i2c::dispatch(&mut Opener { bus, sessions }, frame);
                }
            }
            SessionEvent::Gone(badge) => bus.release(badge),
        }
    }
}
