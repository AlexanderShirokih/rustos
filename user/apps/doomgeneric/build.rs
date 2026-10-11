//! Компилирует движок doomgeneric из исходников стороннего кода. Состав движка - список
//! `SRC_DOOM` из `Makefile` закреплённого коммита без платформенной части `doomgeneric_xlib.c`.
//! Размер `DG_ScreenBuffer` движка задают константы модуля `frame` программы.

#[path = "src/frame.rs"]
mod frame;

use userland_build::{CSources, Error, third_party_dir};

use crate::frame::{FRAME_HEIGHT, FRAME_WIDTH};

const ENTRY: &str = "doomgeneric";
const ENGINE_DIR: &str = "doomgeneric";

const ENGINE: &[&str] = &[
    "dummy.c",
    "am_map.c",
    "doomdef.c",
    "doomstat.c",
    "dstrings.c",
    "d_event.c",
    "d_items.c",
    "d_iwad.c",
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
    "w_file_stdc.c",
    "i_input.c",
    "i_video.c",
    "doomgeneric.c",
];

fn main() -> Result<(), Error> {
    userland_build::emit_linker_script()?;

    let root = third_party_dir(ENTRY)?.join(ENGINE_DIR);
    let sources: Vec<String> = ENGINE
        .iter()
        .map(|source| root.join(source).display().to_string())
        .collect();
    let sources: Vec<&str> = sources.iter().map(String::as_str).collect();

    let width = format!("DOOMGENERIC_RESX={FRAME_WIDTH}");
    let height = format!("DOOMGENERIC_RESY={FRAME_HEIGHT}");
    CSources::new("doomgeneric", &sources)
        .include_dir(&root)
        .define(&width)
        .define(&height)
        .define("NORMALUNIX")
        .define("LINUX")
        .define("_DEFAULT_SOURCE")
        .foreign()
        .compile()
}
