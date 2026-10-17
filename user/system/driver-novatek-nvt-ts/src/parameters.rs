//! Проверка размеров области касаний, числа контактов и модели чипа NT36672A.

/// Адрес начала параметров чипа.
pub const PARAMETERS_ADDRESS: u8 = 0x78;
/// Число байт параметров чипа.
pub const PARAMETERS_LENGTH: u16 = 15;

const WIDTH: usize = 0x04;
const HEIGHT: usize = 0x06;
const MAX_TOUCHES: usize = 0x09;
const IRQ_TYPE: usize = 0x0b;
const WAKE_TYPE: usize = 0x0c;
const CHIP_ID: usize = 0x0e;

const SIZE_MAX: u16 = 4096;
const TOUCHES_MAX: u8 = 10;
const IRQ_TYPE_COUNT: u8 = 4;
const NT36672A_WAKE_TYPE: u8 = 0x01;
const NT36672A_CHIP_ID: u8 = 0x08;

/// Проверенные параметры чипа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parameters {
    width: u16,
    height: u16,
    max_touches: u8,
}

/// Ошибка проверки параметров чипа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParametersError {
    /// Число прочитанных байт отличается от [`PARAMETERS_LENGTH`].
    Length(usize),
    /// Ширина больше 4096.
    Width(u16),
    /// Высота не меньше 4096.
    Height(u16),
    /// Число касаний равно 0 либо больше 10.
    Touches(u8),
    /// Индекс условия прерывания не входит в диапазон 0..4.
    IrqType(u8),
    /// Байт типа пробуждения не равен 0x01.
    WakeType(u8),
    /// Идентификатор чипа не равен 0x08.
    Chip(u8),
}

impl Parameters {
    /// Проверяет байты параметров, прочитанные с [`PARAMETERS_ADDRESS`].
    ///
    /// Отказы:
    /// - `Length` - число байтов отличается от [`PARAMETERS_LENGTH`];
    /// - `Width` - ширина больше 4096;
    /// - `Height` - высота не меньше 4096;
    /// - `Touches` - число касаний равно 0 либо больше 10;
    /// - `IrqType` - индекс условия прерывания не входит в диапазон 0..4;
    /// - `WakeType` - байт 0x0c не равен 0x01;
    /// - `Chip` - байт 0x0e не равен 0x08.
    pub fn parse(bytes: &[u8]) -> Result<Self, ParametersError> {
        let length = bytes.len();
        let bytes: &[u8; PARAMETERS_LENGTH as usize] = bytes
            .try_into()
            .map_err(|_| ParametersError::Length(length))?;
        let byte = |offset: usize| bytes.get(offset).copied().unwrap_or_default();
        let be16 = |offset: usize| u16::from_be_bytes([byte(offset), byte(offset + 1)]);

        let width = be16(WIDTH);
        if width > SIZE_MAX {
            return Err(ParametersError::Width(width));
        }
        let height = be16(HEIGHT);
        if height >= SIZE_MAX {
            return Err(ParametersError::Height(height));
        }
        let max_touches = byte(MAX_TOUCHES);
        if max_touches == 0 || max_touches > TOUCHES_MAX {
            return Err(ParametersError::Touches(max_touches));
        }
        let irq_type = byte(IRQ_TYPE);
        if irq_type >= IRQ_TYPE_COUNT {
            return Err(ParametersError::IrqType(irq_type));
        }
        let wake_type = byte(WAKE_TYPE);
        if wake_type != NT36672A_WAKE_TYPE {
            return Err(ParametersError::WakeType(wake_type));
        }
        let chip = byte(CHIP_ID);
        if chip != NT36672A_CHIP_ID {
            return Err(ParametersError::Chip(chip));
        }

        Ok(Self {
            width,
            height,
            max_touches,
        })
    }

    /// Ширина области касаний в единицах отчёта.
    pub const fn width(&self) -> u16 {
        self.width
    }

    /// Высота области касаний в единицах отчёта.
    pub const fn height(&self) -> u16 {
        self.height
    }

    /// Наибольшее число одновременных касаний, от 1 до 10.
    pub const fn max_touches(&self) -> u8 {
        self.max_touches
    }
}
