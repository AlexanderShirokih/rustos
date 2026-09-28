//! Собирает DTB-блоб для тестов из описания дерева.
//!
//! [`tree!`] записывает дерево синтаксисом DTS и раскрывается в массив [`Token`], пригодный
//! для `const`. [`build`] превращает перечень токенов в блоб. Блок зарезервированных
//! диапазонов памяти в блобе пуст.
//!
//! ```
//! const TREE: &[fdt_test::Token<'static>] = &fdt_test::tree! {
//!     #address-cells = <1>;
//!     soc {
//!         ranges;
//!         serial@2000 {
//!             compatible = "arm,pl011", "arm,primecell";
//!             reg = <0x2000 0x1000>;
//!         };
//!     };
//! };
//!
//! let blob = fdt_test::build(TREE);
//! ```

#![no_std]
#![allow(clippy::cast_possible_truncation)]

extern crate alloc;

use alloc::vec::Vec;

const MAGIC: u32 = 0xD00D_FEED;
const VERSION: u32 = 17;
const LAST_COMPATIBLE_VERSION: u32 = 16;
const HEADER_SIZE: u32 = 40;
const EMPTY_RESERVATION_MAP_SIZE: u32 = 16;

const BEGIN_NODE: u32 = 0x1;
const END_NODE: u32 = 0x2;
const PROP: u32 = 0x3;
const END: u32 = 0x9;

/// Элемент дерева в порядке обхода. [`Token::Node`] открывает узел, [`Token::End`] закрывает
/// последний открытый, свойства узла стоят между ними. Корень открыт заранее, и токены до
/// первого `Node` задают свойства корня.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Token<'a> {
    /// Открывает узел с именем вида `name@unit`.
    Node(&'a str),
    /// Свойство из 32-битных ячеек в порядке big-endian. Пустой срез задаёт свойство без
    /// значения.
    Cells(&'a str, &'a [u32]),
    /// Свойство со списком строк, каждая строка завершается нулём.
    Strings(&'a str, &'a [&'a str]),
    /// Свойство с байтами значения без преобразования.
    Bytes(&'a str, &'a [u8]),
    /// Закрывает последний открытый узел.
    End,
}

/// Собирает блоб из `tokens`. Корень закрывается после последнего токена, остальные узлы
/// закрывает [`Token::End`].
pub fn build(tokens: &[Token<'_>]) -> Vec<u8> {
    let mut strings = Vec::new();
    let mut structure = Vec::new();
    push_u32(&mut structure, BEGIN_NODE);
    push_u32(&mut structure, 0);

    for token in tokens {
        match *token {
            Token::Node(name) => {
                push_u32(&mut structure, BEGIN_NODE);
                structure.extend_from_slice(name.as_bytes());
                structure.push(0);
                align4(&mut structure);
            }
            Token::Cells(name, cells) => {
                let value: Vec<u8> = cells.iter().flat_map(|cell| cell.to_be_bytes()).collect();
                push_prop(&mut structure, &mut strings, name, &value);
            }
            Token::Strings(name, items) => {
                let value: Vec<u8> = items
                    .iter()
                    .flat_map(|item| item.bytes().chain([0]))
                    .collect();
                push_prop(&mut structure, &mut strings, name, &value);
            }
            Token::Bytes(name, value) => push_prop(&mut structure, &mut strings, name, value),
            Token::End => push_u32(&mut structure, END_NODE),
        }
    }

    push_u32(&mut structure, END_NODE);
    push_u32(&mut structure, END);
    wrap(&structure, &strings)
}

fn push_prop(structure: &mut Vec<u8>, strings: &mut Vec<u8>, name: &str, value: &[u8]) {
    push_u32(structure, PROP);
    push_u32(structure, value.len() as u32);
    push_u32(structure, strings.len() as u32);
    structure.extend_from_slice(value);
    align4(structure);
    strings.extend_from_slice(name.as_bytes());
    strings.push(0);
}

fn wrap(structure: &[u8], strings: &[u8]) -> Vec<u8> {
    let off_reservation_map = HEADER_SIZE;
    let off_structure = off_reservation_map + EMPTY_RESERVATION_MAP_SIZE;
    let off_strings = off_structure + structure.len() as u32;
    let total = off_strings + strings.len() as u32;

    let mut blob = Vec::new();
    for word in [
        MAGIC,
        total,
        off_structure,
        off_strings,
        off_reservation_map,
        VERSION,
        LAST_COMPATIBLE_VERSION,
        0,
        strings.len() as u32,
        structure.len() as u32,
    ] {
        push_u32(&mut blob, word);
    }
    blob.extend_from_slice(&[0; EMPTY_RESERVATION_MAP_SIZE as usize]);
    blob.extend_from_slice(structure);
    blob.extend_from_slice(strings);
    blob
}

fn push_u32(buf: &mut Vec<u8>, value: u32) {
    buf.extend_from_slice(&value.to_be_bytes());
}

fn align4(buf: &mut Vec<u8>) {
    while !buf.len().is_multiple_of(4) {
        buf.push(0);
    }
}

/// Записывает дерево синтаксисом DTS и раскрывается в массив [`Token`].
///
/// Свойство записывается как `name = <cells>;`, `name = "a", "b";` либо `name;` без значения.
/// Узел записывается как `name@unit { ... };`. Имя с символами вне идентификатора, например
/// `linux,initrd-start`, записывается строковым литералом. Содержимое вне узлов задаёт свойства
/// корня.
#[macro_export]
macro_rules! tree {
    (@munch [$($out:expr,)*]) => {
        [$($out,)*]
    };
    (@munch [$($out:expr,)*] @close $($rest:tt)*) => {
        $crate::tree!(@munch [$($out,)* $crate::Token::End,] $($rest)*)
    };
    (@munch [$($out:expr,)*]
        $a:ident $(- $b:ident)* $(@ $unit:tt)? { $($inner:tt)* } ; $($rest:tt)*) => {
        $crate::tree!(@munch [$($out,)* $crate::Token::Node(concat!(
            stringify!($a) $(, "-", stringify!($b))* $(, "@", stringify!($unit))?
        )),] $($inner)* @close $($rest)*)
    };
    (@munch [$($out:expr,)*] # $a:ident $(- $b:ident)* = $($rest:tt)*) => {
        $crate::tree!(@value [$($out,)*]
            (concat!("#", stringify!($a) $(, "-", stringify!($b))*)) $($rest)*)
    };
    (@munch [$($out:expr,)*] $a:ident $(- $b:ident)* = $($rest:tt)*) => {
        $crate::tree!(@value [$($out,)*]
            (concat!(stringify!($a) $(, "-", stringify!($b))*)) $($rest)*)
    };
    (@munch [$($out:expr,)*] $name:literal = $($rest:tt)*) => {
        $crate::tree!(@value [$($out,)*] ($name) $($rest)*)
    };
    (@munch [$($out:expr,)*] $a:ident $(- $b:ident)* ; $($rest:tt)*) => {
        $crate::tree!(@munch [$($out,)* $crate::Token::Cells(
            concat!(stringify!($a) $(, "-", stringify!($b))*), &[]
        ),] $($rest)*)
    };
    (@munch [$($out:expr,)*] $name:literal ; $($rest:tt)*) => {
        $crate::tree!(@munch [$($out,)* $crate::Token::Cells($name, &[]),] $($rest)*)
    };
    (@value [$($out:expr,)*] ($name:expr) < $($cell:literal)* > ; $($rest:tt)*) => {
        $crate::tree!(@munch [$($out,)* $crate::Token::Cells($name, &[$($cell),*]),] $($rest)*)
    };
    (@value [$($out:expr,)*] ($name:expr) $($text:literal),+ ; $($rest:tt)*) => {
        $crate::tree!(@munch [$($out,)* $crate::Token::Strings($name, &[$($text),+]),] $($rest)*)
    };
    ($($body:tt)*) => {
        $crate::tree!(@munch [] $($body)*)
    };
}
