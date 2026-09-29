//! Контракт `Platform`, по которому драйвер получает линии прерываний и окна регистров
//! устройств.
//!
//! Клиента и сервис даёт модуль [`platform`]. Область сессии - строка совместимости
//! устройства. Выданные линия и регион отзываются с концом сессии.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(test, allow(clippy::unwrap_in_result))]

use ipc::{
    IntoWireValue, ReceivedResources, WireTyped, WireValue,
    schema::WireType,
    wire::{ENUM_DISCRIMINANT_WIDTH, IpcError, MessageBuf, value},
};
use syscall::kind;

/// Отказ в линии либо окне устройства.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformError {
    /// В описании платформы нет устройства, линии либо окна.
    NotDescribed,
    /// Линию либо регион окна держит другая сессия.
    Taken,
    /// Ресурс описан, но не выдан.
    Unavailable,
}

const VARIANTS: [PlatformError; 3] = [
    PlatformError::NotDescribed,
    PlatformError::Taken,
    PlatformError::Unavailable,
];

impl PlatformError {
    const fn discriminant(self) -> u16 {
        match self {
            Self::NotDescribed => 1,
            Self::Taken => 2,
            Self::Unavailable => 3,
        }
    }
}

impl<'a, R: ReceivedResources> WireValue<'a, R> for PlatformError {
    type Owned = Self;

    fn from_field(data: &'a [u8], _frame: &mut R) -> Result<Self, IpcError> {
        let discriminant = value::decode_discriminant(data)?;
        VARIANTS
            .into_iter()
            .find(|variant| variant.discriminant() == discriminant)
            .ok_or(IpcError::InvalidValue)
    }
}

impl<R: ReceivedResources> IntoWireValue<R> for PlatformError {
    type Read = Self;

    fn write_into(self, frame: &mut MessageBuf<R>, field_id: u8) -> Result<(), IpcError> {
        frame.write_field(field_id, &value::encode_discriminant(self.discriminant()))
    }
}

impl WireTyped for PlatformError {
    const WIRE_TYPE: WireType = WireType::Uint(ENUM_DISCRIMINANT_WIDTH);
}

ipc::wire_value! {
    /// Окно регистров устройства: регион и смещение первого регистра от его начала.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct MmioWindow<M = ipc::wire::Cap<kind::MemoryRegion>> {
        /// Регион, который целиком содержит регистры устройства.
        pub region: M,
        /// Смещение первого регистра устройства от начала региона.
        pub offset: u64,
    }
}

ipc::protocol! {
    /// Ресурсы устройств совместимости, которую называет область сессии.
    pub mod platform {
        name = "Platform";

        /// Выдаёт копию линии прерывания `index` устройства `instance`.
        call fn irq(
            instance: u16,
            index: u16,
        ) -> own Result<ipc::wire::Cap<kind::IrqLine>, PlatformError>;

        /// Выдаёт окно регистров `index` устройства `instance`: копию региона на запись и
        /// смещение регистров в нём. Окна устройств сессии в одном регионе приходят этим
        /// регионом, каждое со своим смещением.
        call fn mmio(instance: u16, index: u16) -> own Result<MmioWindow, PlatformError>;
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use ipc::{
        IntoWireValue, WireValue,
        wire::{Cap, FieldCursor, IpcError, MessageBuf, value},
    };
    use ipc_test::{MockHandle, MockResources};
    use syscall::kind;

    use super::{MmioWindow, PlatformError, VARIANTS, platform};

    const FIELD_ID: u8 = 1;

    const IRQ_LINE: MockHandle<kind::IrqLine> = MockHandle::new(0x40);
    const REGION: MockHandle<kind::MemoryRegion> = MockHandle::new(0x50);

    struct Rtc;

    impl platform::Service<MockResources> for Rtc {
        fn irq(
            &mut self,
            _frame: &MockResources,
            instance: u16,
            index: u16,
        ) -> impl IntoWireValue<MockResources, Read = Result<Cap<kind::IrqLine>, PlatformError>>
        {
            if (instance, index) != (0, 0) {
                return Err(PlatformError::NotDescribed);
            }
            Ok(IRQ_LINE)
        }

        fn mmio(
            &mut self,
            _frame: &MockResources,
            instance: u16,
            index: u16,
        ) -> impl IntoWireValue<MockResources, Read = Result<MmioWindow, PlatformError>> {
            if index != 0 {
                return Err(PlatformError::NotDescribed);
            }
            Ok(MmioWindow {
                region: REGION,
                offset: u64::from(instance) * 0x200,
            })
        }
    }

    fn round_trip(error: PlatformError) -> Result<PlatformError, IpcError> {
        let mut buf = MessageBuf::<()>::new();
        error.write_into(&mut buf, FIELD_ID).expect("write ok");
        buf.finish().expect("finish ok");
        let mut cursor = FieldCursor::new(buf.as_bytes());
        let field = cursor.next_field().expect("decode ok").expect("present");
        PlatformError::from_field(field.data, &mut ())
    }

    #[test]
    fn every_error_round_trips() {
        for variant in VARIANTS {
            assert_eq!(round_trip(variant), Ok(variant));
        }
    }

    #[test]
    fn an_error_outside_the_set_is_rejected() {
        for discriminant in [0, 4, u16::MAX] {
            assert_eq!(
                PlatformError::from_field(&value::encode_discriminant(discriminant), &mut ()),
                Err(IpcError::InvalidValue)
            );
        }
    }

    #[test]
    fn operations_answer_with_the_resource_or_the_refusal() {
        let (client_end, server_end) = ipc_test::pair();
        let client = platform::Client::new(client_end);
        thread::scope(|scope| {
            scope.spawn(|| {
                let mut rtc = Rtc;
                for _ in 0..4 {
                    server_end
                        .serve(|frame| platform::dispatch(&mut rtc, frame))
                        .expect("dispatch ok");
                }
            });
            assert_eq!(client.irq(0, 0), Ok(Ok(IRQ_LINE)));
            assert_eq!(client.irq(1, 0), Ok(Err(PlatformError::NotDescribed)));
            assert_eq!(
                client.mmio(1, 0),
                Ok(Ok(MmioWindow {
                    region: REGION,
                    offset: 0x200,
                }))
            );
            assert_eq!(client.mmio(0, 1), Ok(Err(PlatformError::NotDescribed)));
        });
    }
}
