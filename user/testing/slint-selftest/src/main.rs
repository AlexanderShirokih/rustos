//! Стенд Slint с управляемым ожиданием переноса кадра и вводом через порты ядра.

#![allow(clippy::expect_used, clippy::panic)]

mod stand;

#[allow(
    unsafe_code,
    unreachable_pub,
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::restriction
)]
mod ui {
    slint::include_modules!();
}

pub(crate) use stand::main;
