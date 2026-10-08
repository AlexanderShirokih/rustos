//! Семейство `malloc` библиотеки C на куче процесса и глобальный аллокатор Rust-части
//! программы поверх той же кучи.
//!
//! Перед блоком лежит заголовок с размером и выравниванием, с которыми блок выделен из кучи:
//! `free` и `realloc` восстанавливают их по указателю. Выравнивание блока не меньше
//! `MIN_ALIGN`, заголовок занимает конец первой его доли. `realloc` с нулевым размером
//! освобождает блок и возвращает нулевой указатель.

#![allow(unsafe_code)]

use core::{
    ffi::{c_int, c_void},
    ptr,
};

use crate::errno::{self, Errno};

const MIN_ALIGN: usize = 16;

const HEADER_SIZE: usize = size_of::<Header>();

const _: () = assert!(HEADER_SIZE <= MIN_ALIGN && align_of::<Header>() <= MIN_ALIGN);

runtime::process_heap!();

#[derive(Clone, Copy)]
#[repr(C)]
struct Header {
    size: usize,
    align: usize,
}

impl Header {
    const fn offset(self) -> usize {
        self.align
    }

    const fn payload(self) -> usize {
        self.size - self.offset()
    }

    #[allow(clippy::cast_ptr_alignment)]
    unsafe fn slot(data: *mut u8) -> *mut Self {
        // SAFETY: контракт функции. Данные блока выровнены по `MIN_ALIGN`, и отступ
        // `HEADER_SIZE` от них сохраняет выравнивание `Header`.
        unsafe { data.sub(HEADER_SIZE) }.cast()
    }

    #[allow(clippy::multiple_unsafe_ops_per_block)]
    unsafe fn store(self, data: *mut u8) {
        // SAFETY: вызывающий передаёт данные блока, перед которыми лежат `HEADER_SIZE` байт
        // того же блока; данные выровнены по `MIN_ALIGN`, и это место выровнено под `Header`.
        unsafe { Self::slot(data).write(self) };
    }

    #[allow(clippy::multiple_unsafe_ops_per_block)]
    unsafe fn load(data: *mut u8) -> Self {
        // SAFETY: перед данными блока, выданного `allocate`, лежит заголовок, записанный
        // `allocate` и с тех пор не менявшийся.
        unsafe { Self::slot(data).read() }
    }
}

fn allocate(payload: usize, align: usize) -> *mut u8 {
    let align = align.max(MIN_ALIGN);
    let Some(size) = align.checked_add(payload) else {
        errno::set(Errno::OutOfMemory);
        return ptr::null_mut();
    };
    let header = Header { size, align };
    // SAFETY: `size` не меньше `MIN_ALIGN`, `align` - степень двойки.
    let block = unsafe { runtime::alloc(size, align) };
    if block.is_null() {
        errno::set(Errno::OutOfMemory);
        return ptr::null_mut();
    }
    // SAFETY: блок занимает `size` байт, и отступ `align` оставляет в нём место под данные.
    let data = unsafe { block.add(header.offset()) };
    // SAFETY: `data` отстоит от начала блока на отступ, перед ним `align` байт блока.
    unsafe { header.store(data) };
    data
}

pub(crate) fn duplicate(bytes: &[u8]) -> *mut u8 {
    let data = allocate(bytes.len(), MIN_ALIGN);
    if !data.is_null() {
        // SAFETY: новый блок вмещает `bytes.len()` байт и не пересекается с `bytes`.
        unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len()) };
    }
    data
}

unsafe fn release(data: *mut u8) {
    // SAFETY: контракт функции.
    let header = unsafe { Header::load(data) };
    // SAFETY: начало блока отстоит от данных на отступ заголовка.
    let block = unsafe { data.sub(header.offset()) };
    // SAFETY: блок выдан кучей с этими размером и выравниванием.
    unsafe { runtime::dealloc(block, header.size, header.align) };
}

#[unsafe(no_mangle)]
extern "C" fn malloc(size: usize) -> *mut c_void {
    allocate(size, MIN_ALIGN).cast()
}

#[unsafe(no_mangle)]
extern "C" fn calloc(count: usize, size: usize) -> *mut c_void {
    let Some(total) = count.checked_mul(size) else {
        errno::set(Errno::OutOfMemory);
        return ptr::null_mut();
    };
    let data = allocate(total, MIN_ALIGN);
    if !data.is_null() {
        // SAFETY: блок только что выделен под `total` байт.
        unsafe { data.write_bytes(0, total) };
    }
    data.cast()
}

#[unsafe(no_mangle)]
unsafe extern "C" fn free(data: *mut c_void) {
    if !data.is_null() {
        // SAFETY: контракт `free`.
        unsafe { release(data.cast()) };
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn realloc(data: *mut c_void, size: usize) -> *mut c_void {
    let old = data.cast::<u8>();
    if old.is_null() {
        return malloc(size);
    }
    if size == 0 {
        // SAFETY: контракт `realloc`.
        unsafe { release(old) };
        return ptr::null_mut();
    }
    // SAFETY: контракт `realloc`.
    let header = unsafe { Header::load(old) };
    let new = allocate(size, header.align);
    if new.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: оба блока живы, не пересекаются и вмещают меньший из двух размеров.
    unsafe { ptr::copy_nonoverlapping(old, new, header.payload().min(size)) };
    // SAFETY: контракт `realloc`; данные перенесены.
    unsafe { release(old) };
    new.cast()
}

#[unsafe(no_mangle)]
extern "C" fn aligned_alloc(align: usize, size: usize) -> *mut c_void {
    if !align.is_power_of_two() {
        errno::set(Errno::InvalidArgument);
        return ptr::null_mut();
    }
    allocate(size, align).cast()
}

#[unsafe(no_mangle)]
unsafe extern "C" fn posix_memalign(out: *mut *mut c_void, align: usize, size: usize) -> c_int {
    if !align.is_power_of_two() || align < size_of::<*mut c_void>() {
        return Errno::InvalidArgument.code();
    }
    let data = allocate(size, align);
    if data.is_null() {
        return Errno::OutOfMemory.code();
    }
    // SAFETY: контракт `posix_memalign`.
    unsafe { out.write(data.cast()) };
    0
}
