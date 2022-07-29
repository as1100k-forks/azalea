use azalea_buf::McBuf;
use azalea_core::BlockPos;
use packet_macros::ClientboundGamePacket;

#[derive(Clone, Debug, McBuf, ClientboundGamePacket)]
pub struct ClientboundSetDefaultSpawnPositionPacket {
    pub pos: BlockPos,
    pub angle: f32,
}
