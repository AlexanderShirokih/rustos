//! Компилирует интерфейс `ui/physics.slint` с глифами шрифта Inter из реестра стороннего кода,
//! встроенными SDF для программного рендерера. Каталог шрифта доступен интерфейсу как библиотека
//! `@inter`.

use core::error::Error;
use std::collections::HashMap;

const FONT_ENTRY: &str = "inter";
const FONT_LIBRARY: &str = "inter";
const FONT_DIR: &str = "extras/ttf";

fn main() -> Result<(), Box<dyn Error>> {
    userland_build::emit_linker_script()?;

    let fonts = userland_build::third_party_dir(FONT_ENTRY)?.join(FONT_DIR);
    let config = slint_build::CompilerConfiguration::new()
        .embed_resources(slint_build::EmbedResourcesKind::EmbedForSoftwareRenderer)
        .with_sdf_fonts(true)
        .with_library_paths(HashMap::from([(FONT_LIBRARY.to_string(), fonts)]));
    slint_build::compile_with_config("ui/physics.slint", config)?;
    Ok(())
}
