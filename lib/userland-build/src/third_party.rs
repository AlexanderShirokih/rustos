//! Каталог закреплённого стороннего кода.
//!
//! Корень каталогов записей задаёт переменная `MOLECULAR_THIRD_PARTY`. Записи должна разложить
//! `cargo xtask third-party` до сборки пакета.

use std::path::PathBuf;

use crate::{Error, error::cargo_variable};

const THIRD_PARTY_VARIABLE: &str = "MOLECULAR_THIRD_PARTY";

/// Каталог записи `name` реестра стороннего кода. Пакет пересобирается при смене корня.
pub fn third_party_dir(name: &str) -> Result<PathBuf, Error> {
    println!("cargo:rerun-if-env-changed={THIRD_PARTY_VARIABLE}");
    cargo_variable(THIRD_PARTY_VARIABLE).map(|root| PathBuf::from(root).join(name))
}
