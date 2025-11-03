use crate::fdt::DeviceTree;
use core::arch::asm;
use core::ptr::write_volatile;

/// Форматы пикселей, встречающиеся в simple-framebuffer/qualcomm fb.
#[derive(Copy, Clone, Debug)]
pub enum PixelFormat {
    Argb8888,
    Xrgb8888,
    Rgb565,
    Unknown,
}

/// Информация о фреймбуфере из DTB (физ. адрес и геометрия).
#[derive(Clone, Copy, Debug)]
pub struct FramebufferInfo {
    pub paddr: usize,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub bpp: u32,
    pub format: PixelFormat,
}

/// Описатель уже отмапленного/готового к записи буфера.
#[derive(Copy, Clone, Debug)]
pub struct Framebuffer {
    pub ptr: *mut u8, // виртуальный адрес начала буфера
    pub width: usize,
    pub height: usize,
    pub stride_bytes: usize, // длина строки в байтах
    pub bpp: usize,          // бит на пиксель
    pub format: PixelFormat,
}

impl Framebuffer {
    /// Записывает один пиксель с учётом bpp/stride и формата (LE).
    #[inline]
    pub fn put_pixel(&mut self, x: usize, y: usize, color: u32) {
        if x >= self.width || y >= self.height {
            return;
        }
        unsafe {
            match self.bpp {
                32 => {
                    let off = y * self.stride_bytes + x * 4;
                    let word = pack32_le(self.format, color);
                    // volatile, чтобы компилятор не выкинул запись в MMIO/FB
                    write_volatile(self.ptr.add(off) as *mut u32, word);
                }
                16 => {
                    let off = y * self.stride_bytes + x * 2;
                    let c565 = rgb888_to_rgb565(color);
                    write_volatile(self.ptr.add(off) as *mut u16, c565);
                }
                _ => {}
            }
        }
    }

    /// Заливает весь буфер цветом (простая, но не самая быстрая реализация).
    pub fn clear(&mut self, color: u32) {
        for y in 0..self.height {
            for x in 0..self.width {
                self.put_pixel(x, y, color);
            }
        }
    }

    /// Заливает прямоугольник `[x..x+w) × [y..y+h)` без выхода за границы.
    pub fn fill_rect(&mut self, x: usize, y: usize, w: usize, h: usize, color: u32) {
        let x2 = (x + w).min(self.width);
        let y2 = (y + h).min(self.height);
        let bpp_bytes = (self.bpp / 8).max(1);
        for row in y..y2 {
            let row_base = row * self.stride_bytes + x * bpp_bytes;
            for col in x..x2 {
                unsafe {
                    match self.bpp {
                        32 => {
                            let word = pack32_le(self.format, color);
                            write_volatile(
                                self.ptr.add(row_base + (col - x) * 4) as *mut u32,
                                word,
                            );
                        }
                        16 => write_volatile(
                            self.ptr.add(row_base + (col - x) * 2) as *mut u16,
                            rgb888_to_rgb565(color),
                        ),
                        _ => {}
                    }
                }
            }
        }
    }
}

#[inline]
fn rgb888_to_rgb565(c: u32) -> u16 {
    let r = ((c >> 16) & 0xFF) as u16;
    let g = ((c >> 8) & 0xFF) as u16;
    let b = (c & 0xFF) as u16;
    ((r >> 3) << 11) | ((g >> 2) << 5) | (b >> 3)
}

/// Упаковка ARGB в порядок байт памяти для 32-битных форматов (LE).
#[inline]
fn pack32_le(fmt: PixelFormat, argb: u32) -> u32 {
    let a = (argb >> 24) as u8;
    let r = (argb >> 16) as u8;
    let g = (argb >> 8) as u8;
    let b = (argb >> 0) as u8;
    match fmt {
        // a8r8g8b8: в памяти на LE будет B, G, R, A
        PixelFormat::Argb8888 => u32::from_le_bytes([b, g, r, a]),
        PixelFormat::Xrgb8888 => u32::from_le_bytes([b, g, r, 0xFF]),
        PixelFormat::Unknown => u32::from_le_bytes([b, g, r, a]),
        _ => u32::from_le_bytes([b, g, r, a]),
    }
}

/// Принудительно очищает кэш данных для области фреймбуфера до PoC.
#[inline(always)]
pub fn flush_framebuffer(ptr: *const u8, len: usize) {
    let line_size: usize = 64; // типичное значение
    let mut p = (ptr as usize) & !(line_size - 1);
    let end = (ptr as usize) + len;

    unsafe {
        while p < end {
            // cvac = Clean by VA to PoC
            asm!("dc cvac, {p}", p = in(reg) p);
            p += line_size;
        }
        // Барьер, чтобы предыдущие dc-операции завершились и стали видимы.
        asm!("dsb ish");
    }
}

fn parse_pixel_format(fmt: &[u8]) -> (PixelFormat, u32) {
    if fmt.starts_with(b"a8r8g8b8") {
        (PixelFormat::Argb8888, 32)
    } else if fmt.starts_with(b"x8r8g8b8") {
        (PixelFormat::Xrgb8888, 32)
    } else if fmt.starts_with(b"r8g8b8a8") {
        // В некоторых DT встречается как r8g8b8a8 для XRGB.
        (PixelFormat::Xrgb8888, 32)
    } else if fmt.starts_with(b"r5g6b5") {
        (PixelFormat::Rgb565, 16)
    } else {
        (PixelFormat::Unknown, 0)
    }
}

/// Ищет simple-framebuffer в DTB и возвращает параметры фреймбуфера.
pub unsafe fn find_in_dtb(device_tree: &DeviceTree) -> Option<FramebufferInfo> {
    let node = device_tree.find_first(|n| {
        let name = n.name();
        if name == b"framebuffer"
            || name.starts_with(b"framebuffer@")
            || name.starts_with(b"simple-framebuffer")
        {
            return true;
        }
        if let Some(compat) = n.get_prop(b"compatible") {
            return compat.starts_with(b"simple-framebuffer");
        }
        false
    })?;

    let mut reg_base: usize = 0;
    let mut width: u32 = 0;
    let mut height: u32 = 0;
    let mut stride: u32 = 0;
    let mut bpp: u32 = 0;
    let mut fmt = PixelFormat::Unknown;

    let mut it = node.props();
    while let Some(p) = it.next() {
        if p.name == b"reg" {
            if p.value.len() >= 16 {
                let addr_hi = read_be_u32(p.value, 0) as u64;
                let addr_lo = read_be_u32(p.value, 4) as u64;
                reg_base = ((addr_hi << 32) | addr_lo) as usize;
            } else if p.value.len() >= 8 {
                reg_base = read_be_u32(p.value, 0) as usize;
            }
        } else if p.name == b"width" && p.value.len() >= 4 {
            width = read_be_u32(p.value, 0);
        } else if p.name == b"height" && p.value.len() >= 4 {
            height = read_be_u32(p.value, 0);
        } else if (p.name == b"stride" || p.name == b"line_length") && p.value.len() >= 4 {
            stride = read_be_u32(p.value, 0);
        } else if (p.name == b"format" || p.name == b"pixel_format") && !p.value.is_empty() {
            // Строка формата может быть с NUL в конце — отрежем его.
            let nul = p
                .value
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(p.value.len());
            let (pf, bits) = parse_pixel_format(&p.value[..nul]);
            fmt = pf;
            if bits != 0 {
                bpp = bits;
            }
        } else if p.name == b"bits-per-pixel" && p.value.len() >= 4 {
            bpp = read_be_u32(p.value, 0);
        }
    }

    // Если stride не указан — вычисляем от width*bpp.
    if reg_base != 0 && width != 0 && height != 0 && (stride != 0 || bpp != 0) {
        let final_stride = if stride != 0 {
            stride
        } else {
            width * (bpp / 8)
        };
        let final_bpp = if bpp != 0 { bpp } else { 32 };
        return Some(FramebufferInfo {
            paddr: reg_base,
            width,
            height,
            stride: final_stride,
            bpp: final_bpp,
            format: fmt,
        });
    }

    None
}

#[inline(always)]
fn read_be_u32(buf: &[u8], off: usize) -> u32 {
    let b0 = *buf.get(off).unwrap_or(&0) as u32;
    let b1 = *buf.get(off + 1).unwrap_or(&0) as u32;
    let b2 = *buf.get(off + 2).unwrap_or(&0) as u32;
    let b3 = *buf.get(off + 3).unwrap_or(&0) as u32;
    (b0 << 24) | (b1 << 16) | (b2 << 8) | b3
}
