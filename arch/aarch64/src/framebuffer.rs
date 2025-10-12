#![allow(dead_code)]
use core::ptr::{write_volatile, copy_nonoverlapping};

#[derive(Copy, Clone, Debug)]
pub enum PixelFormat { Argb8888, Xrgb8888, Rgb565, Unknown }

#[derive(Copy, Clone, Debug)]
pub struct Framebuffer {
    pub ptr: *mut u8,
    pub width: usize,
    pub height: usize,
    pub stride_bytes: usize,
    pub bpp: usize,
    pub format: PixelFormat,
}

impl Framebuffer {
    #[inline]
    pub fn put_pixel(&mut self, x: usize, y: usize, color: u32) {
        if x >= self.width || y >= self.height { return; }
        unsafe {
            match self.bpp {
                32 => {
                    let off = y * self.stride_bytes + x * 4;
                    write_volatile(self.ptr.add(off) as *mut u32, color);
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

    pub fn clear(&mut self, color: u32) {
        for y in 0..self.height {
            for x in 0..self.width {
                self.put_pixel(x, y, color);
            }
        }
    }

    pub fn fill_rect(&mut self, x: usize, y: usize, w: usize, h: usize, color: u32) {
        let x2 = (x + w).min(self.width);
        let y2 = (y + h).min(self.height);
        let bpp_bytes = (self.bpp / 8).max(1);
        for row in y..y2 {
            let row_base = row * self.stride_bytes + x * bpp_bytes;
            for col in x..x2 {
                unsafe {
                    match self.bpp {
                        32 => write_volatile(self.ptr.add(row_base + (col - x) * 4) as *mut u32, color),
                        16 => write_volatile(self.ptr.add(row_base + (col - x) * 2) as *mut u16, rgb888_to_rgb565(color)),
                        _ => {}
                    }
                }
            }
        }
    }

    pub unsafe fn blit_from(&mut self, src: *const u32, stride_pixels: usize) {
        if self.bpp != 32 { return; }
        let rows = self.height;
        for y in 0..rows {
            let dst = self.ptr.add(y * self.stride_bytes) as *mut u32;
            let s = src.add(y * stride_pixels);
            copy_nonoverlapping(s, dst, self.width);
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

#[inline(always)]
pub unsafe fn flush_framebuffer(ptr: *const u8, len: usize) {
    // Clean data cache by VA to point of unification; conservative approach for visibility.
    let line_size: usize = 64; // typical; exact value could be read from CTR_EL0, but avoid sysregs here.
    let mut p = (ptr as usize) & !(line_size - 1);
    let end = (ptr as usize) + len;
    while p < end {
        core::arch::asm!("dc cvau, {p}", p = in(reg) p);
        p += line_size;
    }
    core::arch::asm!("dsb ish");
    core::arch::asm!("isb");
}
