//! Общие хелперы для построения DTB-blob в тестах (`#[cfg(test)]`).
//!
//! Строковые смещения вычисляются программно через [`StringPool`], а не
//! хардкодятся.
#![cfg(test)]
// Разные тестовые модули используют разные подмножества хелперов.
#![allow(dead_code)]

extern crate alloc;

use alloc::vec::Vec;

/// Пул строк FDT: интернирует имена свойств и возвращает их смещения.
#[derive(Default)]
pub struct StringPool {
    bytes: Vec<u8>,
}

impl StringPool {
    pub fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    /// Интернирует строку и возвращает её смещение в пуле.
    pub fn intern(&mut self, name: &str) -> u32 {
        let off = self.bytes.len() as u32;
        self.bytes.extend_from_slice(name.as_bytes());
        self.bytes.push(0);
        off
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

pub fn push_u32(buf: &mut Vec<u8>, value: u32) {
    buf.extend_from_slice(&value.to_be_bytes());
}

pub fn push_prop_bytes(buf: &mut Vec<u8>, name_off: u32, value: &[u8]) {
    push_u32(buf, 0x3);
    push_u32(buf, value.len() as u32);
    push_u32(buf, name_off);
    buf.extend_from_slice(value);
    align4(buf);
}

pub fn align4(buf: &mut Vec<u8>) {
    while !buf.len().is_multiple_of(4) {
        buf.push(0);
    }
}

/// Собирает финальный FDT-blob из блока структур и пула строк.
pub fn build_fdt(structure: &[u8], strings: &[u8]) -> Vec<u8> {
    let off_mem_rsvmap = 40u32;
    let off_struct = off_mem_rsvmap + 16; // 16 = размер пустого rsvmap-терминатора
    let off_strings = off_struct + structure.len() as u32;
    let total = off_strings + strings.len() as u32;

    let mut buf = Vec::new();
    push_u32(&mut buf, 0xD00D_FEED);
    push_u32(&mut buf, total);
    push_u32(&mut buf, off_struct);
    push_u32(&mut buf, off_strings);
    push_u32(&mut buf, off_mem_rsvmap);
    push_u32(&mut buf, 17);
    push_u32(&mut buf, 16);
    push_u32(&mut buf, 0);
    push_u32(&mut buf, strings.len() as u32);
    push_u32(&mut buf, structure.len() as u32);

    buf.extend_from_slice(&[0; 16]);
    buf.extend_from_slice(structure);
    buf.extend_from_slice(strings);
    buf
}

/// Строит `/chosen`-DTB с опциональными `linux,initrd-start/-end`.
///
/// Каждое значение задаётся сырыми байтами; пустой вектор означает "свойство
/// отсутствует" (для проверки частично заданных диапазонов). Строковые
/// смещения вычисляются через [`StringPool`].
pub fn build_chosen_dtb_with_payload(initrd: Option<(Vec<u8>, Vec<u8>)>) -> Vec<u8> {
    let mut pool = StringPool::new();
    let start_off = pool.intern("linux,initrd-start");
    let end_off = pool.intern("linux,initrd-end");

    let mut structure = Vec::new();
    push_u32(&mut structure, 0x1);
    push_u32(&mut structure, 0x0);

    push_u32(&mut structure, 0x1);
    structure.extend_from_slice(b"chosen\0");
    align4(&mut structure);

    if let Some((start, end)) = initrd {
        if !start.is_empty() {
            push_prop_bytes(&mut structure, start_off, &start);
        }
        if !end.is_empty() {
            push_prop_bytes(&mut structure, end_off, &end);
        }
    }

    push_u32(&mut structure, 0x2);
    push_u32(&mut structure, 0x2);
    push_u32(&mut structure, 0x9);

    build_fdt(&structure, pool.bytes())
}

/// Удобная обёртка: оба initrd-свойства как 64-битные значения (или ни одного).
pub fn build_chosen_dtb(initrd: Option<(u64, u64)>) -> Vec<u8> {
    build_chosen_dtb_with_payload(
        initrd.map(|(start, end)| (start.to_be_bytes().to_vec(), end.to_be_bytes().to_vec())),
    )
}

/// Как [`build_chosen_dtb`], но значения 32-битные.
pub fn build_chosen_dtb_32(initrd: Option<(u32, u32)>) -> Vec<u8> {
    build_chosen_dtb_with_payload(
        initrd.map(|(start, end)| (start.to_be_bytes().to_vec(), end.to_be_bytes().to_vec())),
    )
}

/// Открывает узел: `begin-node` + имя (с null + выравнивание).
pub fn begin_node(structure: &mut Vec<u8>, name: &str) {
    push_u32(structure, 0x1);
    structure.extend_from_slice(name.as_bytes());
    structure.push(0);
    align4(structure);
}

/// Закрывает узел (`end-node`).
pub fn end_node(structure: &mut Vec<u8>) {
    push_u32(structure, 0x2);
}

/// Завершает дерево (`end`).
pub fn end_tree(structure: &mut Vec<u8>) {
    push_u32(structure, 0x9);
}
