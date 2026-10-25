//! Окна плиток TLMM, отображённые в процесс контроллера.

use executor::PortChannel;
use kernel_objects::{MmioMapping, SessionTransport};
use mmio::MappedWindows;
use pinctrl_msm::Tile;
use platform_abi::{DeviceId, Node, WindowRefusal, platform};

pub(crate) struct Windows {
    north: MmioMapping,
    center: MmioMapping,
    south: MmioMapping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    Unnamed(Tile),
    Window(WindowRefusal),
}

impl Windows {
    pub(crate) async fn take(
        platform: &platform::Client<PortChannel<SessionTransport>>,
        id: &DeviceId,
        node: &Node<'_>,
    ) -> Result<Self, Refusal> {
        let region = async |tile: Tile| {
            let resource = tile.resource(node).ok_or(Refusal::Unnamed(tile))?;
            platform
                .map_mmio(id, resource)
                .await
                .map_err(Refusal::Window)
        };

        Ok(Self {
            north: region(Tile::North).await?,
            center: region(Tile::Center).await?,
            south: region(Tile::South).await?,
        })
    }
}

impl MappedWindows<Tile> for Windows {
    fn mapping(&self, tile: Tile) -> &MmioMapping {
        match tile {
            Tile::North => &self.north,
            Tile::Center => &self.center,
            Tile::South => &self.south,
        }
    }
}
