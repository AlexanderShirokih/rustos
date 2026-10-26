//! Компилирует движок doomgeneric из исходников стороннего кода и кладёт в `OUT_DIR` IWAD
//! программы. Состав движка - список `SRC_DOOM` из `Makefile` закреплённого коммита без
//! платформенной части `doomgeneric_xlib.c` и класса файлов `w_file_stdc.c`. Класс файлов
//! WAD и проверку IWAD в `d_iwad.c` даёт программа. Размер `DG_ScreenBuffer` движка задают
//! константы модуля `frame` программы.

#[path = "src/frame.rs"]
mod frame;
#[path = "build/iwad.rs"]
mod iwad;

use core::error::Error;
use std::{
    env, fs,
    path::{Path, PathBuf},
};

use userland_build::{CSources, third_party_dir};

use crate::frame::{FRAME_HEIGHT, FRAME_WIDTH};

const ENTRY: &str = "doomgeneric";
const ENGINE_DIR: &str = "doomgeneric";
const IWAD_ENTRY: &str = "freedoom";
const IWAD_SOURCE: &str = "freedoom-0.13.0/freedoom1.wad";
const IWAD_OUTPUT: &str = "freedoom1.wad";
const IWAD_SEARCH: &str = "d_iwad.c";
const IWAD_EXISTS: &str = "M_FileExists=DG_IwadExists";

const ENGINE: &[&str] = &[
    "dummy.c",
    "am_map.c",
    "doomdef.c",
    "doomstat.c",
    "dstrings.c",
    "d_event.c",
    "d_items.c",
    "d_loop.c",
    "d_main.c",
    "d_mode.c",
    "d_net.c",
    "f_finale.c",
    "f_wipe.c",
    "g_game.c",
    "hu_lib.c",
    "hu_stuff.c",
    "info.c",
    "i_cdmus.c",
    "i_endoom.c",
    "i_joystick.c",
    "i_scale.c",
    "i_sound.c",
    "i_system.c",
    "i_timer.c",
    "memio.c",
    "m_argv.c",
    "m_bbox.c",
    "m_cheat.c",
    "m_config.c",
    "m_controls.c",
    "m_fixed.c",
    "m_menu.c",
    "m_misc.c",
    "m_random.c",
    "p_ceilng.c",
    "p_doors.c",
    "p_enemy.c",
    "p_floor.c",
    "p_inter.c",
    "p_lights.c",
    "p_map.c",
    "p_maputl.c",
    "p_mobj.c",
    "p_plats.c",
    "p_pspr.c",
    "p_saveg.c",
    "p_setup.c",
    "p_sight.c",
    "p_spec.c",
    "p_switch.c",
    "p_telept.c",
    "p_tick.c",
    "p_user.c",
    "r_bsp.c",
    "r_data.c",
    "r_draw.c",
    "r_main.c",
    "r_plane.c",
    "r_segs.c",
    "r_sky.c",
    "r_things.c",
    "sha1.c",
    "sounds.c",
    "statdump.c",
    "st_lib.c",
    "st_stuff.c",
    "s_sound.c",
    "tables.c",
    "v_video.c",
    "wi_stuff.c",
    "w_checksum.c",
    "w_file.c",
    "w_main.c",
    "w_wad.c",
    "z_zone.c",
    "i_input.c",
    "i_video.c",
    "doomgeneric.c",
];

fn main() -> Result<(), Box<dyn Error>> {
    userland_build::emit_linker_script()?;

    let root = third_party_dir(ENTRY)?.join(ENGINE_DIR);
    let engine: Vec<String> = ENGINE
        .iter()
        .map(|source| root.join(source).display().to_string())
        .collect();
    let search = [root.join(IWAD_SEARCH).display().to_string()];
    compile("doomgeneric", &root, &engine, &[])?;
    compile("doomgeneric-iwad", &root, &search, &[IWAD_EXISTS])?;

    write_iwad()
}

fn compile(
    name: &str,
    root: &Path,
    sources: &[String],
    definitions: &[&str],
) -> Result<(), Box<dyn Error>> {
    let sources: Vec<&str> = sources.iter().map(String::as_str).collect();
    let width = format!("DOOMGENERIC_RESX={FRAME_WIDTH}");
    let height = format!("DOOMGENERIC_RESY={FRAME_HEIGHT}");
    definitions
        .iter()
        .fold(CSources::new(name, &sources), |sources, definition| {
            sources.define(definition)
        })
        .include_dir(root)
        .define(&width)
        .define(&height)
        .define("NORMALUNIX")
        .define("LINUX")
        .define("_DEFAULT_SOURCE")
        .foreign()
        .compile()?;
    Ok(())
}

fn write_iwad() -> Result<(), Box<dyn Error>> {
    let source = third_party_dir(IWAD_ENTRY)?.join(IWAD_SOURCE);
    println!("cargo:rerun-if-changed={}", source.display());
    let wad = fs::read(&source)
        .map_err(|error| format!("failed to read {}: {error}", source.display()))?;
    let output = PathBuf::from(env::var("OUT_DIR")?).join(IWAD_OUTPUT);
    fs::write(&output, iwad::first_episode(&wad)?)
        .map_err(|error| format!("failed to write {}: {error}", output.display()))?;
    Ok(())
}
