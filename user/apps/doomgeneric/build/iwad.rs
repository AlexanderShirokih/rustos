//! Урезание IWAD до первого эпизода.
//!
//! Из каталога лампов уходят карты эпизодов со второго. Демо, записанное на удалённом
//! эпизоде, получает данные первого демо первого эпизода.

use core::fmt::{self, Debug, Display, Formatter};

const MAGIC: &[u8] = b"IWAD";
const HEADER_SIZE: usize = 12;
const ENTRY_SIZE: usize = 16;
const NAME_SIZE: usize = 8;
const KEPT_EPISODE: u8 = 1;
const MAP_LUMPS: [&[u8]; 10] = [
    b"THINGS",
    b"LINEDEFS",
    b"SIDEDEFS",
    b"VERTEXES",
    b"SEGS",
    b"SSECTORS",
    b"NODES",
    b"SECTORS",
    b"REJECT",
    b"BLOCKMAP",
];
const DEMO_EPISODE_OFFSET: usize = 2;

pub(crate) enum IwadError {
    NotIwad,
    Truncated,
    NoReplacementDemo,
    TooLarge,
}

struct Lump<'a> {
    name: [u8; NAME_SIZE],
    data: &'a [u8],
}

impl Display for IwadError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotIwad => f.write_str("file is not an IWAD"),
            Self::Truncated => f.write_str("IWAD header, directory or lump is truncated"),
            Self::NoReplacementDemo => {
                write!(f, "IWAD has no demo of episode {KEPT_EPISODE}")
            }
            Self::TooLarge => f.write_str("trimmed IWAD exceeds 4 GiB"),
        }
    }
}

impl Debug for IwadError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(self, f)
    }
}

impl core::error::Error for IwadError {}

pub(crate) fn first_episode(wad: &[u8]) -> Result<Vec<u8>, IwadError> {
    let lumps = lumps(wad)?;
    let mut kept: Vec<&Lump<'_>> = Vec::with_capacity(lumps.len());
    let mut in_removed_map = false;
    for lump in &lumps {
        let name = trimmed(&lump.name);
        if let Some(episode) = map_episode(name) {
            in_removed_map = episode != KEPT_EPISODE;
        } else if !MAP_LUMPS.contains(&name) {
            in_removed_map = false;
        }
        if !in_removed_map {
            kept.push(lump);
        }
    }

    // Заставка ищет демо по имени, поэтому имя демо удалённого эпизода остаётся в каталоге.
    let replacement = kept
        .iter()
        .find(|lump| demo_episode(lump) == Some(KEPT_EPISODE))
        .map(|lump| lump.data);

    let mut out = vec![0; HEADER_SIZE];
    let mut directory = Vec::with_capacity(kept.len() * ENTRY_SIZE);
    for lump in &kept {
        let data = match demo_episode(lump) {
            Some(episode) if episode != KEPT_EPISODE => {
                replacement.ok_or(IwadError::NoReplacementDemo)?
            }
            _ => lump.data,
        };
        directory.extend_from_slice(&le32(out.len())?);
        directory.extend_from_slice(&le32(data.len())?);
        directory.extend_from_slice(&lump.name);
        out.extend_from_slice(data);
    }

    let directory_offset = out.len();
    out.extend_from_slice(&directory);
    let header = [MAGIC, &le32(kept.len())?, &le32(directory_offset)?].concat();
    out.splice(..HEADER_SIZE, header);
    Ok(out)
}

fn lumps(wad: &[u8]) -> Result<Vec<Lump<'_>>, IwadError> {
    if wad.get(..MAGIC.len()) != Some(MAGIC) {
        return Err(IwadError::NotIwad);
    }
    let count = read_le32(wad, 4)?;
    let directory = read_le32(wad, 8)?;
    (0..count)
        .map(|index| {
            let entry = directory + index * ENTRY_SIZE;
            let offset = read_le32(wad, entry)?;
            let size = read_le32(wad, entry + 4)?;
            let name = wad
                .get(entry + 8..entry + ENTRY_SIZE)
                .and_then(|name| name.try_into().ok())
                .ok_or(IwadError::Truncated)?;
            let data = wad.get(offset..offset + size).ok_or(IwadError::Truncated)?;
            Ok(Lump { name, data })
        })
        .collect()
}

fn trimmed(name: &[u8; NAME_SIZE]) -> &[u8] {
    let end = name.iter().position(|&byte| byte == 0).unwrap_or(NAME_SIZE);
    name.get(..end).unwrap_or(name)
}

fn map_episode(name: &[u8]) -> Option<u8> {
    match name {
        [b'E', episode @ b'1'..=b'9', b'M', b'1'..=b'9'] => Some(episode - b'0'),
        _ => None,
    }
}

fn demo_episode(lump: &Lump<'_>) -> Option<u8> {
    // Заголовок демо формата 1.4+ содержит версию, сложность, эпизод и карту.
    trimmed(&lump.name)
        .starts_with(b"DEMO")
        .then(|| lump.data.get(DEMO_EPISODE_OFFSET).copied())
        .flatten()
}

fn read_le32(bytes: &[u8], at: usize) -> Result<usize, IwadError> {
    let value = bytes
        .get(at..at + 4)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or(IwadError::Truncated)?;
    usize::try_from(value).map_err(|_| IwadError::TooLarge)
}

fn le32(value: usize) -> Result<[u8; 4], IwadError> {
    u32::try_from(value)
        .map(u32::to_le_bytes)
        .map_err(|_| IwadError::TooLarge)
}
