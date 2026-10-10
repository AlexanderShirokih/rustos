//! Компилирует интерфейс `ui/journal.slint` с глифами моноширинного шрифта DejaVu Sans Mono из
//! реестра стороннего кода, встроенными SDF для программного рендерера. Каталог шрифтов доступен
//! интерфейсу как библиотека `@dejavu`.

use core::error::Error;
use std::collections::HashMap;

const FONT_ENTRY: &str = "dejavu-fonts";
const FONT_LIBRARY: &str = "dejavu";
const FONT_DIR: &str = "dejavu-fonts-ttf-2.37/ttf";

fn main() -> Result<(), Box<dyn Error>> {
    userland_build::emit_linker_script()?;

    let fonts = userland_build::third_party_dir(FONT_ENTRY)?.join(FONT_DIR);
    let config = slint_build::CompilerConfiguration::new()
        .embed_resources(slint_build::EmbedResourcesKind::EmbedForSoftwareRenderer)
        .with_sdf_fonts(true)
        .with_library_paths(HashMap::from([(FONT_LIBRARY.to_string(), fonts)]));
    slint_build::compile_with_config("ui/journal.slint", config)?;
    Ok(())
}
