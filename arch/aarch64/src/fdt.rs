#![allow(dead_code)]
#![allow(clippy::missing_safety_doc)]

// Minimal FDT (DTB) reader for no_std environment.
// Based on Linux Devicetree v0.3 spec. Big-endian fields in header and structure.

use core::mem::size_of;

const FDT_MAGIC: u32 = 0xD00D_FEED;

#[inline(always)]
fn be32(x: u32) -> u32 { u32::from_be(x) }

#[repr(C)]
#[derive(Clone, Copy)]
struct FdtHeader {
    magic: u32,
    totalsize: u32,
    off_dt_struct: u32,
    off_dt_strings: u32,
    off_mem_rsvmap: u32,
    version: u32,
    last_comp_version: u32,
    boot_cpuid_phys: u32,
    size_dt_strings: u32,
    size_dt_struct: u32,
}

#[repr(u32)]
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum Token {
    FDT_BEGIN_NODE = 1,
    FDT_END_NODE = 2,
    FDT_PROP = 3,
    FDT_NOP = 4,
    FDT_END = 9,
}

#[inline(always)]
fn align4(v: usize) -> usize { (v + 3) & !3 }

#[derive(Clone, Copy, Debug)]
pub struct FramebufferInfo {
    pub paddr: usize,
    pub size: usize,
    pub width: u32,
    pub height: u32,
    pub stride: u32, // bytes per line
    pub bpp: u32,
    pub format: PixelFormat,
}

#[derive(Clone, Copy, Debug)]
pub enum PixelFormat { Argb8888, Xrgb8888, Rgb565, Unknown }

fn parse_pixel_format(fmt: &[u8]) -> (PixelFormat, u32) {
    // Common strings: "a8r8g8b8", "x8r8g8b8", "r8g8b8a8", "r5g6b5"
    if fmt.starts_with(b"a8r8g8b8") { (PixelFormat::Argb8888, 32) }
    else if fmt.starts_with(b"x8r8g8b8") { (PixelFormat::Xrgb8888, 32) }
    else if fmt.starts_with(b"r8g8b8a8") { (PixelFormat::Xrgb8888, 32) } // treat as XRGB
    else if fmt.starts_with(b"r5g6b5") { (PixelFormat::Rgb565, 16) }
    else { (PixelFormat::Unknown, 0) }
}

pub unsafe fn find_framebuffer(dtb_ptr: usize) -> Option<FramebufferInfo> {
    if dtb_ptr == 0 { return None; }

    let hdr = &*(dtb_ptr as *const FdtHeader);
    if be32(hdr.magic) != FDT_MAGIC { return None; }

    let struct_off = be32(hdr.off_dt_struct) as usize;
    let strings_off = be32(hdr.off_dt_strings) as usize;
    let struct_base = (dtb_ptr + struct_off) as *const u8;
    let strings_base = (dtb_ptr + strings_off) as *const u8;

    let mut p = struct_base as usize;

    // Track current node and whether it's a candidate framebuffer node
    let mut in_fb_node = false;
    let mut have_simplefb_compatible = false;

    // Parsed properties
    let mut reg_base: usize = 0; let mut reg_size: usize = 0;
    let mut width: u32 = 0; let mut height: u32 = 0; let mut stride: u32 = 0; let mut bpp: u32 = 0; let mut fmt = PixelFormat::Unknown;

    loop {
        let token = be32(*(p as *const u32));
        p += size_of::<u32>();
        match token {
            t if t == Token::FDT_BEGIN_NODE as u32 => {
                // Read node name (C-string), 4-byte aligned
                let mut q = p;
                while unsafe { *(q as *const u8) } != 0 { q += 1; }
                let name_start = p as *const u8;
                let name_len = q - p;
                let name = core::slice::from_raw_parts(name_start, name_len);
                p = align4(q + 1);

                // Decide if this node is interesting
                in_fb_node = false;
                have_simplefb_compatible = false;

                // Heuristics: node name equals "framebuffer" or starts with "framebuffer@" or "simple-framebuffer"
                if name == b"framebuffer" || name.starts_with(b"framebuffer@") || name.starts_with(b"simple-framebuffer") {
                    in_fb_node = true;
                }
            }
            t if t == Token::FDT_END_NODE as u32 => {
                if in_fb_node {
                    // If we collected enough, return
                    let size = if reg_size != 0 { reg_size } else { (stride as usize) * (height as usize) };
                    if reg_base != 0 && width != 0 && height != 0 && (stride != 0 || bpp != 0) {
                        let final_stride = if stride != 0 { stride } else { width * (bpp / 8) };
                        let final_bpp = if bpp != 0 { bpp } else { 32 };
                        let info = FramebufferInfo {
                            paddr: reg_base,
                            size,
                            width,
                            height,
                            stride: final_stride,
                            bpp: final_bpp,
                            format: fmt,
                        };
                        return Some(info);
                    }
                }
                in_fb_node = false;
                have_simplefb_compatible = false;
            }
            t if t == Token::FDT_PROP as u32 => {
                let len = be32(*(p as *const u32)) as usize; p += size_of::<u32>();
                let nameoff = be32(*(p as *const u32)) as usize; p += size_of::<u32>();
                let data = p as *const u8; p = align4(p + len);

                let name = cstr_at(strings_base, nameoff);
                // Track compatible for simple-framebuffer
                if name == b"compatible" {
                    if bytes_eq_prefix(unsafe { core::slice::from_raw_parts(data, len) }, b"simple-framebuffer") {
                        in_fb_node = true; have_simplefb_compatible = true;
                    }
                }

                if !in_fb_node { continue; }

                if name == b"reg" {
                    // We assume 64-bit address + length pairs when possible; many Android DTBs still use 32-bit here though.
                    if len >= 16 {
                        let addr_hi = be32(unsafe { *(data as *const u32) }) as u64;
                        let addr_lo = be32(unsafe { *((data as usize + 4) as *const u32) }) as u64;
                        let size_hi = be32(unsafe { *((data as usize + 8) as *const u32) }) as u64;
                        let size_lo = be32(unsafe { *((data as usize + 12) as *const u32) }) as u64;
                        reg_base = ((addr_hi << 32) | addr_lo) as usize;
                        reg_size = ((size_hi << 32) | size_lo) as usize;
                    } else if len >= 8 {
                        let addr = be32(unsafe { *(data as *const u32) }) as usize;
                        let size = be32(unsafe { *((data as usize + 4) as *const u32) }) as usize;
                        reg_base = addr; reg_size = size;
                    }
                } else if name == b"width" && len >= 4 {
                    width = be32(unsafe { *(data as *const u32) });
                } else if name == b"height" && len >= 4 {
                    height = be32(unsafe { *(data as *const u32) });
                } else if (name == b"stride" || name == b"line_length") && len >= 4 {
                    stride = be32(unsafe { *(data as *const u32) });
                } else if (name == b"format" || name == b"pixel_format") && len > 0 {
                    // parse string until NUL
                    let bytes = unsafe { core::slice::from_raw_parts(data, len) };
                    let nul = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
                    let (pf, bits) = parse_pixel_format(&bytes[..nul]);
                    fmt = pf; if bits != 0 { bpp = bits; }
                } else if name == b"bits-per-pixel" && len >= 4 {
                    bpp = be32(unsafe { *(data as *const u32) });
                }
            }
            t if t == Token::FDT_NOP as u32 => {},
            t if t == Token::FDT_END as u32 => break,
            _ => break,
        }
    }

    None
}

#[inline(always)]
fn cstr_at(base: *const u8, off: usize) -> &'static [u8] {
    let mut p = (base as usize) + off;
    let start = p as *const u8;
    while unsafe { *(p as *const u8) } != 0 { p += 1; }
    let len = p - (start as usize);
    unsafe { core::slice::from_raw_parts(start, len) }
}

#[inline(always)]
fn bytes_eq_prefix(a: &[u8], b: &[u8]) -> bool {
    if a.len() < b.len() { return false; }
    &a[..b.len()] == b
}
