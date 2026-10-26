//! Компилирует интерфейс `ui/calculator.slint` с глифами шрифта Inter из реестра стороннего кода,
//! встроенными SDF для программного рендерера. Каталог шрифта доступен интерфейсу как библиотека
//! `@inter`.

use core::error::Error;
use std::collections::HashMap;

const FONT_ENTRY: &str = "inter";
const FONT_LIBRARY: &str = "inter";
const FONT_DIR: &str = "extras/ttf";

const SDF_FONT_SIZES: &str = "108";

fn main() -> Result<(), Box<dyn Error>> {
    userland_build::emit_linker_script()?;

    // SAFETY: окружение читает и меняет только этот поток.
    #[allow(unsafe_code)]
    unsafe {
        // Slint строит SDF под 2/3 наибольшего размера из `SLINT_FONT_SIZES`, но не крупнее
        // 72 px. 108 px дают эти 72 px.
        std::env::set_var("SLINT_FONT_SIZES", SDF_FONT_SIZES);
    }

    let fonts = userland_build::third_party_dir(FONT_ENTRY)?.join(FONT_DIR);
    let config = slint_build::CompilerConfiguration::new()
        .embed_resources(slint_build::EmbedResourcesKind::EmbedForSoftwareRenderer)
        .with_sdf_fonts(true)
        .with_library_paths(HashMap::from([(FONT_LIBRARY.to_string(), fonts)]));
    slint_build::compile_with_config("ui/calculator.slint", config)?;

    Ok(())
}
