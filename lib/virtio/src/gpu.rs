//! Команды virtio-gpu 2D и разбор ответов устройства.
//!
//! Команда и ответ начинаются заголовком из 24 байт с типом. Все поля little-endian.

use crate::queue::Buffer;

const HEADER_BYTES: usize = 24;
/// Число выводов в ответе на запрос информации о выводах.
pub const SCANOUTS_MAX: usize = 16;
const SCANOUT_BYTES: usize = 24;
const DISPLAY_INFO_BYTES: usize = HEADER_BYTES + SCANOUTS_MAX * SCANOUT_BYTES;

const COMMAND_GET_DISPLAY_INFO: u32 = 0x0100;
const COMMAND_RESOURCE_CREATE_2D: u32 = 0x0101;
const COMMAND_SET_SCANOUT: u32 = 0x0103;
const COMMAND_RESOURCE_FLUSH: u32 = 0x0104;
const COMMAND_TRANSFER_TO_HOST_2D: u32 = 0x0105;
const COMMAND_RESOURCE_ATTACH_BACKING: u32 = 0x0106;
const RESPONSE_OK_NODATA: u32 = 0x1100;
const RESPONSE_OK_DISPLAY_INFO: u32 = 0x1101;

/// Формат пикселя ресурса.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Format {
    /// Байты пикселя B, G, R и неиспользуемый. Слово little-endian имеет вид `0x00RRGGBB`.
    B8G8R8X8Unorm = 2,
}

/// Прямоугольник в пикселях.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Команда GPU 2D.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command<'a> {
    /// Запрос геометрии и включённости выводов.
    GetDisplayInfo,
    /// Создание ресурса на стороне устройства.
    ResourceCreate2d {
        resource_id: u32,
        format: Format,
        width: u32,
        height: u32,
    },
    /// Привязка памяти к ресурсу: по элементу на каждый непрерывный участок `backing`.
    ResourceAttachBacking {
        resource_id: u32,
        backing: &'a [Buffer],
    },
    /// Вывод прямоугольника ресурса на вывод `scanout_id`.
    SetScanout {
        scanout_id: u32,
        resource_id: u32,
        rect: Rect,
    },
    /// Перенос прямоугольника из памяти ресурса на устройство, `offset` - смещение начала
    /// прямоугольника в памяти ресурса.
    TransferToHost2d {
        resource_id: u32,
        rect: Rect,
        offset: u64,
    },
    /// Обновление прямоугольника ресурса на выводах.
    ResourceFlush { resource_id: u32, rect: Rect },
}

/// Отказ кодирования команды или разбора ответа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuError {
    /// Буфер короче закодированной команды.
    BufferTooSmall,
    /// Ответ короче формата своего типа.
    Truncated,
    /// Тип ответа - ошибка устройства либо не соответствует команде.
    UnexpectedResponse(u32),
}

/// Вывод из ответа на [`Command::GetDisplayInfo`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Scanout {
    /// Геометрия вывода.
    pub rect: Rect,
    /// Вывод включён.
    pub enabled: bool,
}

/// Ответ на [`Command::GetDisplayInfo`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayInfo {
    /// Выводы по номеру.
    pub scanouts: [Scanout; SCANOUTS_MAX],
}

struct Encoder<'a> {
    buffer: &'a mut [u8],
    length: usize,
}

impl Command<'_> {
    /// Кодирует команду в начало `buffer` и возвращает её длину.
    pub fn encode(&self, buffer: &mut [u8]) -> Result<usize, GpuError> {
        let mut encoder = Encoder { buffer, length: 0 };
        encoder.header(self.command_type())?;
        match *self {
            Self::GetDisplayInfo => {}
            Self::ResourceCreate2d {
                resource_id,
                format,
                width,
                height,
            } => {
                encoder.words(&[resource_id, format as u32, width, height])?;
            }
            Self::ResourceAttachBacking {
                resource_id,
                backing,
            } => {
                let entries = u32::try_from(backing.len()).map_err(|_| GpuError::BufferTooSmall)?;
                encoder.words(&[resource_id, entries])?;
                for entry in backing {
                    encoder.put(&entry.address.to_le_bytes())?;
                    encoder.words(&[entry.length, 0])?;
                }
            }
            Self::SetScanout {
                scanout_id,
                resource_id,
                rect,
            } => {
                encoder.rect(rect)?;
                encoder.words(&[scanout_id, resource_id])?;
            }
            Self::TransferToHost2d {
                resource_id,
                rect,
                offset,
            } => {
                encoder.rect(rect)?;
                encoder.put(&offset.to_le_bytes())?;
                encoder.words(&[resource_id, 0])?;
            }
            Self::ResourceFlush { resource_id, rect } => {
                encoder.rect(rect)?;
                encoder.words(&[resource_id, 0])?;
            }
        }
        Ok(encoder.length)
    }

    /// Длина буфера ответа на команду.
    pub fn response_bytes(&self) -> usize {
        match self {
            Self::GetDisplayInfo => DISPLAY_INFO_BYTES,
            _ => HEADER_BYTES,
        }
    }

    /// Проверяет, что `response` - успешный ответ на эту команду.
    pub fn check_response(&self, response: &[u8]) -> Result<(), GpuError> {
        let response_type = read_word(response, 0)?;
        if response_type != self.response_type() {
            return Err(GpuError::UnexpectedResponse(response_type));
        }
        if response.len() < self.response_bytes() {
            return Err(GpuError::Truncated);
        }
        Ok(())
    }

    fn command_type(&self) -> u32 {
        match self {
            Self::GetDisplayInfo => COMMAND_GET_DISPLAY_INFO,
            Self::ResourceCreate2d { .. } => COMMAND_RESOURCE_CREATE_2D,
            Self::ResourceAttachBacking { .. } => COMMAND_RESOURCE_ATTACH_BACKING,
            Self::SetScanout { .. } => COMMAND_SET_SCANOUT,
            Self::TransferToHost2d { .. } => COMMAND_TRANSFER_TO_HOST_2D,
            Self::ResourceFlush { .. } => COMMAND_RESOURCE_FLUSH,
        }
    }

    fn response_type(&self) -> u32 {
        match self {
            Self::GetDisplayInfo => RESPONSE_OK_DISPLAY_INFO,
            _ => RESPONSE_OK_NODATA,
        }
    }
}

impl DisplayInfo {
    /// Разбирает ответ на [`Command::GetDisplayInfo`].
    pub fn parse(response: &[u8]) -> Result<Self, GpuError> {
        Command::GetDisplayInfo.check_response(response)?;

        let mut scanouts = [Scanout::default(); SCANOUTS_MAX];
        for (number, scanout) in scanouts.iter_mut().enumerate() {
            let offset = HEADER_BYTES + number * SCANOUT_BYTES;
            let word = |field: usize| read_word(response, offset + 4 * field);
            let rect = Rect {
                x: word(0)?,
                y: word(1)?,
                width: word(2)?,
                height: word(3)?,
            };
            *scanout = Scanout {
                rect,
                enabled: word(4)? != 0,
            };
        }
        Ok(Self { scanouts })
    }
}

impl Encoder<'_> {
    fn put(&mut self, bytes: &[u8]) -> Result<(), GpuError> {
        let end = self.length + bytes.len();
        let target = self
            .buffer
            .get_mut(self.length..end)
            .ok_or(GpuError::BufferTooSmall)?;
        target.copy_from_slice(bytes);
        self.length = end;
        Ok(())
    }

    fn words(&mut self, words: &[u32]) -> Result<(), GpuError> {
        words
            .iter()
            .try_for_each(|word| self.put(&word.to_le_bytes()))
    }

    fn header(&mut self, command_type: u32) -> Result<(), GpuError> {
        // Тип, флаги, номер ограды из двух слов, контекст, номер кольца с выравниванием.
        self.words(&[command_type, 0, 0, 0, 0, 0])
    }

    fn rect(&mut self, rect: Rect) -> Result<(), GpuError> {
        self.words(&[rect.x, rect.y, rect.width, rect.height])
    }
}

fn read_word(bytes: &[u8], offset: usize) -> Result<u32, GpuError> {
    let word = bytes
        .get(offset..offset + 4)
        .and_then(|word| word.try_into().ok());
    word.map(u32::from_le_bytes).ok_or(GpuError::Truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(bytes: &[u8]) -> Vec<u32> {
        bytes
            .chunks(4)
            .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
            .collect()
    }

    #[test]
    fn display_info_reports_enabled_scanout() {
        let mut response = vec![0; DISPLAY_INFO_BYTES];
        response[..4].copy_from_slice(&RESPONSE_OK_DISPLAY_INFO.to_le_bytes());
        let scanout = [0u32, 0, 1280, 800, 1, 0].map(u32::to_le_bytes).concat();
        response[HEADER_BYTES..HEADER_BYTES + SCANOUT_BYTES].copy_from_slice(&scanout);

        let info = DisplayInfo::parse(&response).unwrap();

        let rect = Rect {
            x: 0,
            y: 0,
            width: 1280,
            height: 800,
        };
        assert_eq!(
            info.scanouts[0],
            Scanout {
                rect,
                enabled: true
            }
        );
        assert!(!info.scanouts[1].enabled);
    }

    #[test]
    fn error_response_names_its_type() {
        let rect = Rect {
            x: 0,
            y: 0,
            width: 16,
            height: 16,
        };
        let transfer = Command::TransferToHost2d {
            resource_id: 1,
            rect,
            offset: 0,
        };
        let mut response = vec![0; HEADER_BYTES];
        response[..4].copy_from_slice(&0x1205u32.to_le_bytes());

        let result = transfer.check_response(&response);

        assert_eq!(result, Err(GpuError::UnexpectedResponse(0x1205)));
    }

    #[test]
    fn short_response_is_truncated_only_for_expected_type() {
        let mut header = [0; HEADER_BYTES];
        header[..4].copy_from_slice(&RESPONSE_OK_DISPLAY_INFO.to_le_bytes());
        let truncated = DisplayInfo::parse(&header);
        header[..4].copy_from_slice(&0x1200u32.to_le_bytes());
        let error = DisplayInfo::parse(&header);

        assert_eq!(truncated, Err(GpuError::Truncated));
        assert_eq!(error, Err(GpuError::UnexpectedResponse(0x1200)));
    }

    #[test]
    fn attach_backing_lists_each_segment() {
        let backing = [
            Buffer {
                address: 0xa000_0000,
                length: 8192,
            },
            Buffer {
                address: 0xb000_0000,
                length: 4096,
            },
        ];
        let command = Command::ResourceAttachBacking {
            resource_id: 7,
            backing: &backing,
        };
        let mut buffer = [0; 64];

        let length = command.encode(&mut buffer).unwrap();

        assert_eq!(length, 64);
        assert_eq!(words(&buffer[..4]), [COMMAND_RESOURCE_ATTACH_BACKING]);
        assert_eq!(
            words(&buffer[HEADER_BYTES..]),
            [7, 2, 0xa000_0000, 0, 8192, 0, 0xb000_0000, 0, 4096, 0]
        );
    }

    #[test]
    fn transfer_places_rect_offset_and_resource_after_header() {
        let rect = Rect {
            x: 1,
            y: 2,
            width: 3,
            height: 4,
        };
        let command = Command::TransferToHost2d {
            resource_id: 9,
            rect,
            offset: 0x1_0000_0010,
        };
        let mut buffer = [0; 56];

        let length = command.encode(&mut buffer).unwrap();

        assert_eq!(length, 56);
        assert_eq!(
            words(&buffer),
            [
                COMMAND_TRANSFER_TO_HOST_2D,
                0,
                0,
                0,
                0,
                0,
                1,
                2,
                3,
                4,
                0x10,
                1,
                9,
                0
            ]
        );
    }
}
