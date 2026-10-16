//! Геометрия диска.

use block_abi::BlockInfo;
use virtio::block::{Config, SECTOR_BYTES, feature};

const TRANSFER_LIMIT_BYTES: u32 = 64 * 1024;

/// Геометрия устройства вне обслуживаемой.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeometryError {
    /// Размер блока не степень двойки либо меньше 512 байт.
    BlockSize(u32),
    /// Ёмкость меньше одного блока.
    NoBlocks,
    /// Предел передачи меньше одного блока.
    TransferMax(u32),
}

/// Возвращает размер блока, число блоков и предел передачи диска с признаками `accepted`,
/// согласованными с устройством, и конфигурацией устройства `config`.
///
/// Размер блока - `blk_size` при [`feature::BLK_SIZE`], иначе 512 байт. Число блоков -
/// ёмкость, делённая на размер блока в секторах. Предел передачи - меньшее из 64 KiB и
/// `size_max` при [`feature::SIZE_MAX`], округлённое вниз до размера блока.
///
/// Отказы:
/// - `BlockSize` - размер блока не степень двойки либо меньше 512 байт;
/// - `NoBlocks` - ёмкость меньше одного блока;
/// - `TransferMax` - предел передачи меньше одного блока.
pub fn geometry(accepted: u64, config: Config) -> Result<BlockInfo, GeometryError> {
    let block_size = if accepted & feature::BLK_SIZE != 0 {
        config.blk_size
    } else {
        SECTOR_BYTES
    };
    if !block_size.is_power_of_two() || block_size < SECTOR_BYTES {
        return Err(GeometryError::BlockSize(block_size));
    }

    let blocks = config.capacity / u64::from(block_size / SECTOR_BYTES);
    if blocks == 0 {
        return Err(GeometryError::NoBlocks);
    }

    let limit = if accepted & feature::SIZE_MAX != 0 {
        config.size_max.min(TRANSFER_LIMIT_BYTES)
    } else {
        TRANSFER_LIMIT_BYTES
    };
    let transfer_max = limit / block_size * block_size;
    if transfer_max == 0 {
        return Err(GeometryError::TransferMax(limit));
    }

    Ok(BlockInfo {
        block_size,
        blocks,
        transfer_max,
    })
}
