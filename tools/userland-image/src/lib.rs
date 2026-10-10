//! Собирает `userland.img` на host: строит программы, перечисленные манифестом образа, и
//! кодирует их вместе с планом композиции в файл образа.
//!
//! [`build_userland`] принимает путь манифеста, каталог сборки, исходники стандартной
//! библиотеки и таргет, а возвращает путь готового файла. [`manifest_file_sources`] отдаёт
//! источники файлов манифеста. [`userland_image_seeds`] отдаёт валидные образы затравками
//! корпуса фаззера формата.
//!
//! Крейт работает на host. Разбор и кодирование самого формата лежат в `userland-image`.

#![allow(clippy::std_instead_of_core)]

mod host;
#[cfg(test)]
mod manifest;
mod plan;
mod seeds;
mod units;

pub use host::{
    BuildEnvironment, ExtraFile, USERLAND_WORKSPACE, UserlandTarget, build_userland,
    manifest_file_sources,
};
pub use seeds::{ImageSeed, userland_image_seeds};
