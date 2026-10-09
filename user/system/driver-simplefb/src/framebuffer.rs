use display_abi::{Area, DisplayDimension, DisplayError, SURFACE_PIXEL_BYTES};
use platform_abi::{Description, DescriptionError, DeviceId, Node};

/// Значение `compatible` узла буфера кадра, который обслуживает драйвер.
pub const SIMPLE_FRAMEBUFFER_COMPATIBLE: &str = "simple-framebuffer";

const SURFACE_PIXEL: usize = SURFACE_PIXEL_BYTES as usize;

const COPY_CHUNK_PIXELS: usize = 64;

const CHANNEL_BITS: u32 = 8;

const PIXEL_FORMATS: [PixelFormat; 11] = [
    PixelFormat::new("r5g6b5", 2, [(11, 5), (5, 6), (0, 5), (0, 0)]),
    PixelFormat::new("r5g5b5a1", 2, [(11, 5), (6, 5), (1, 5), (0, 1)]),
    PixelFormat::new("x1r5g5b5", 2, [(10, 5), (5, 5), (0, 5), (0, 0)]),
    PixelFormat::new("a1r5g5b5", 2, [(10, 5), (5, 5), (0, 5), (15, 1)]),
    PixelFormat::new("r8g8b8", 3, [(16, 8), (8, 8), (0, 8), (0, 0)]),
    PixelFormat::new("x8r8g8b8", 4, [(16, 8), (8, 8), (0, 8), (0, 0)]),
    PixelFormat::new("a8r8g8b8", 4, [(16, 8), (8, 8), (0, 8), (24, 8)]),
    PixelFormat::new("x8b8g8r8", 4, [(0, 8), (8, 8), (16, 8), (0, 0)]),
    PixelFormat::new("a8b8g8r8", 4, [(0, 8), (8, 8), (16, 8), (24, 8)]),
    PixelFormat::new("x2r10g10b10", 4, [(20, 10), (10, 10), (0, 10), (0, 0)]),
    PixelFormat::new("a2r10g10b10", 4, [(20, 10), (10, 10), (0, 10), (30, 2)]),
];

/// Отказ в открытии буфера кадра.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Описание платформы не содержит доступного узла устройства.
    NotDescribed,
    /// Узел не совместим с [`SIMPLE_FRAMEBUFFER_COMPATIBLE`], задаёт память свойством
    /// `memory-region` либо задаёт формат пикселя вне биндинга `simple-framebuffer`.
    Unsupported,
    /// Свойства узла отсутствуют, не разобраны или задают кадр, который не помещается в строки
    /// либо в окно устройства.
    Malformed,
}

/// Размер экрана, шаг строки и формат пикселя буфера кадра. Ширина и высота больше нуля,
/// шаг строки не меньше ширины экрана, умноженной на размер пикселя формата.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Framebuffer {
    dimension: DisplayDimension,
    stride: u32,
    format: PixelFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PixelFormat {
    name: &'static str,
    bytes: usize,
    red: Field,
    green: Field,
    blue: Field,
    alpha: Field,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Field {
    shift: u32,
    bits: u32,
}

impl From<DescriptionError> for Refusal {
    fn from(_: DescriptionError) -> Self {
        Self::Malformed
    }
}

impl Framebuffer {
    /// Читает узел устройства `device` описания `description`, как [`Framebuffer::new`]. Если
    /// описание не содержит доступного узла `device`, возвращает `NotDescribed`.
    pub fn open(description: &Description<'_>, device: &DeviceId) -> Result<Self, Refusal> {
        let node = description.node(device).ok_or(Refusal::NotDescribed)?;
        Self::new(&node)
    }

    /// Читает `width`, `height`, `stride` и `format` узла `node`.
    ///
    /// Если узел не совместим с [`SIMPLE_FRAMEBUFFER_COMPATIBLE`], задаёт свойство
    /// `memory-region` или формат вне биндинга `simple-framebuffer`, возвращает `Unsupported`.
    /// Если свойство отсутствует или не разобрано, ширина либо высота равна нулю или шаг строки
    /// меньше строки пикселей формата, возвращает `Malformed`.
    pub fn new(node: &Node<'_>) -> Result<Self, Refusal> {
        let mut compatible = node.strings("compatible")?.ok_or(Refusal::Unsupported)?;
        if !compatible.any(|name| name == SIMPLE_FRAMEBUFFER_COMPATIBLE) {
            return Err(Refusal::Unsupported);
        }
        if node.bytes("memory-region").is_some() {
            return Err(Refusal::Unsupported);
        }

        let width = node.u32("width")?.ok_or(Refusal::Malformed)?;
        let height = node.u32("height")?.ok_or(Refusal::Malformed)?;
        let stride = node.u32("stride")?.ok_or(Refusal::Malformed)?;
        let name = node.string("format")?.ok_or(Refusal::Malformed)?;
        let format = PIXEL_FORMATS
            .into_iter()
            .find(|format| format.name == name)
            .ok_or(Refusal::Unsupported)?;

        let row = u64::from(width) * format.bytes as u64;
        if width == 0 || height == 0 || u64::from(stride) < row {
            return Err(Refusal::Malformed);
        }
        Ok(Self {
            dimension: DisplayDimension { width, height },
            stride,
            format,
        })
    }

    /// Размер экрана в пикселях.
    pub fn dimension(&self) -> DisplayDimension {
        self.dimension
    }

    /// Шаг строки буфера кадра в байтах.
    pub fn stride(&self) -> u32 {
        self.stride
    }

    /// Имя формата пикселя из свойства `format` узла.
    pub fn format(&self) -> &'static str {
        self.format.name
    }

    /// Если кадр размером в шаг строки, умноженный на высоту, не помещается в окно размером
    /// `window_size` байт, возвращает `Malformed`.
    pub fn fits(&self, window_size: u64) -> Result<(), Refusal> {
        let frame_size = u64::from(self.stride) * u64::from(self.dimension.height);
        if frame_size <= window_size {
            Ok(())
        } else {
            Err(Refusal::Malformed)
        }
    }

    /// Переносит пиксели поверхности внутри `area` на экран. Байты экрана вне `area` не
    /// меняются.
    ///
    /// Формат с раскладкой пикселя поверхности - четыре байта, красный, зелёный и синий по восемь
    /// бит со сдвигами 16, 8 и 0 - получает строки поверхности без изменений, и старший байт
    /// пикселя поверхности становится неиспользуемым байтом либо альфой. Строку переносит
    /// вызов `copy(surface_offset, screen_offset, len)`.
    ///
    /// Остальные форматы получают закодированные пиксели с непрозрачной альфой и нулевыми
    /// неиспользуемыми битами. `read` читает байты поверхности с заданного смещения, `write`
    /// записывает байты на экран с заданного смещения.
    ///
    /// Если [`DisplayDimension::check_area`] отвергает `area`, возвращает `OutOfBounds` без
    /// вызова `read`, `write` и `copy`. Если одно из них возвращает ошибку, перенос прекращается
    /// с `OutOfBounds`.
    pub fn present<E>(
        &self,
        area: Area,
        mut read: impl FnMut(usize, &mut [u8]) -> Result<(), E>,
        mut write: impl FnMut(usize, &[u8]) -> Result<(), E>,
        mut copy: impl FnMut(usize, usize, usize) -> Result<(), E>,
    ) -> Result<(), DisplayError> {
        self.dimension.check_area(area)?;

        let width = usize::try_from(area.width).map_err(|_| DisplayError::OutOfBounds)?;
        let surface_layout = self.format.has_surface_layout();
        // `check_area` ограничивает `area.y + area.height` высотой экрана.
        for y in area.y..area.y + area.height {
            let surface_row =
                row_offset(y, area.x, SURFACE_PIXEL, self.dimension.surface_stride())?;
            let screen_row = row_offset(y, area.x, self.format.bytes, u64::from(self.stride))?;
            if surface_layout {
                copy(surface_row, screen_row, width * SURFACE_PIXEL)
                    .map_err(|_| DisplayError::OutOfBounds)?;
                continue;
            }
            match self.format.bytes {
                2 => self.encode_row::<2, E>(width, surface_row, screen_row, &mut read, &mut write),
                3 => self.encode_row::<3, E>(width, surface_row, screen_row, &mut read, &mut write),
                4 => self.encode_row::<4, E>(width, surface_row, screen_row, &mut read, &mut write),
                _ => Err(DisplayError::OutOfBounds),
            }?;
        }
        Ok(())
    }

    fn encode_row<const BYTES: usize, E>(
        &self,
        width: usize,
        surface_row: usize,
        screen_row: usize,
        read: &mut impl FnMut(usize, &mut [u8]) -> Result<(), E>,
        write: &mut impl FnMut(usize, &[u8]) -> Result<(), E>,
    ) -> Result<(), DisplayError> {
        let mut surface_chunk = [[0u8; SURFACE_PIXEL]; COPY_CHUNK_PIXELS];
        let mut screen_chunk = [[0u8; BYTES]; COPY_CHUNK_PIXELS];
        for first in (0..width).step_by(COPY_CHUNK_PIXELS) {
            let count = COPY_CHUNK_PIXELS.min(width - first);
            let source = surface_chunk
                .get_mut(..count)
                .ok_or(DisplayError::OutOfBounds)?;
            let target = screen_chunk
                .get_mut(..count)
                .ok_or(DisplayError::OutOfBounds)?;
            read(
                surface_row + first * SURFACE_PIXEL,
                source.as_flattened_mut(),
            )
            .map_err(|_| DisplayError::OutOfBounds)?;
            for (pixel, encoded) in source.iter().zip(target.iter_mut()) {
                // Биндинг задаёт пиксель экрана словом в порядке little-endian.
                let value = self.format.encode(*pixel).to_le_bytes();
                *encoded = *value.first_chunk().ok_or(DisplayError::OutOfBounds)?;
            }
            write(screen_row + first * BYTES, target.as_flattened())
                .map_err(|_| DisplayError::OutOfBounds)?;
        }
        Ok(())
    }
}

impl PixelFormat {
    const fn new(
        name: &'static str,
        bytes: usize,
        [red, green, blue, alpha]: [(u32, u32); 4],
    ) -> Self {
        Self {
            name,
            bytes,
            red: Field::new(red),
            green: Field::new(green),
            blue: Field::new(blue),
            alpha: Field::new(alpha),
        }
    }

    fn has_surface_layout(&self) -> bool {
        self.bytes == SURFACE_PIXEL
            && self.red == Field::new((16, CHANNEL_BITS))
            && self.green == Field::new((8, CHANNEL_BITS))
            && self.blue == Field::new((0, CHANNEL_BITS))
    }

    fn encode(&self, surface_pixel: [u8; SURFACE_PIXEL]) -> u32 {
        let [blue, green, red, _] = surface_pixel.map(u32::from);
        self.red.encode(red)
            | self.green.encode(green)
            | self.blue.encode(blue)
            | self.alpha.opaque()
    }
}

impl Field {
    const fn new((shift, bits): (u32, u32)) -> Self {
        Self { shift, bits }
    }

    fn encode(self, channel: u32) -> u32 {
        // Широкое поле повторяет старшие биты канала в младших, и `0xFF` становится максимумом
        // поля.
        let value = if self.bits <= CHANNEL_BITS {
            channel >> (CHANNEL_BITS - self.bits)
        } else {
            (channel << (self.bits - CHANNEL_BITS)) | (channel >> (2 * CHANNEL_BITS - self.bits))
        };
        value << self.shift
    }

    fn opaque(self) -> u32 {
        ((1 << self.bits) - 1) << self.shift
    }
}

fn row_offset(y: u32, x: u32, pixel_bytes: usize, stride: u64) -> Result<usize, DisplayError> {
    let pixel = u64::from(x) * pixel_bytes as u64;
    u64::from(y)
        .checked_mul(stride)
        .and_then(|row| row.checked_add(pixel))
        .and_then(|offset| usize::try_from(offset).ok())
        .ok_or(DisplayError::OutOfBounds)
}
