// i don't know the actual name of this packet, i couldn't find it in the source code

use crate::mc_buf::{McBufReadable, McBufWritable, Readable, Writable};
use async_trait::async_trait;
use azalea_chat::component::Component;
use packet_macros::{GamePacket, McBufReadable, McBufWritable};
use tokio::io::AsyncRead;
use uuid::Uuid;

#[derive(Clone, Debug, GamePacket)]
pub struct ClientboundPlayerInfoPacket {
    pub action: Action,
}

#[derive(Clone, Debug)]
pub enum Action {
    AddPlayer(Vec<AddPlayer>),
    UpdateGameMode(Vec<UpdateGameMode>),
    UpdateLatency(Vec<UpdateLatency>),
    UpdateDisplayName(Vec<UpdateDisplayName>),
    RemovePlayer(Vec<RemovePlayer>),
}

#[derive(Clone, Debug, McBufReadable, McBufWritable)]
pub struct PlayerProperty {
    name: String,
    value: String,
    signature: Option<String>,
}

#[derive(Clone, Debug, McBufReadable, McBufWritable)]
pub struct AddPlayer {
    uuid: Uuid,
    properties: Vec<PlayerProperty>,
    #[varint]
    gamemode: u32,
    #[varint]
    ping: i32,
    display_name: Option<Component>,
}

#[derive(Clone, Debug, McBufReadable, McBufWritable)]
pub struct UpdateGameMode {
    uuid: Uuid,
    #[varint]
    gamemode: u32,
}

#[derive(Clone, Debug, McBufReadable, McBufWritable)]
pub struct UpdateLatency {
    uuid: Uuid,
    #[varint]
    ping: i32,
}

#[derive(Clone, Debug, McBufReadable, McBufWritable)]
pub struct UpdateDisplayName {
    uuid: Uuid,
    display_name: Option<Component>,
}
#[derive(Clone, Debug, McBufReadable, McBufWritable)]
pub struct RemovePlayer {
    uuid: Uuid,
}

#[async_trait]
impl McBufReadable for Action {
    async fn read_into<R>(buf: &mut R) -> Result<Self, String>
    where
        R: AsyncRead + std::marker::Unpin + std::marker::Send,
    {
        let id = buf.read_byte().await?;
        Ok(match id {
            0 => Action::AddPlayer(Vec::<AddPlayer>::read_into(buf).await?),
            1 => Action::UpdateGameMode(Vec::<UpdateGameMode>::read_into(buf).await?),
            2 => Action::UpdateLatency(Vec::<UpdateLatency>::read_into(buf).await?),
            3 => Action::UpdateDisplayName(Vec::<UpdateDisplayName>::read_into(buf).await?),
            4 => Action::RemovePlayer(Vec::<RemovePlayer>::read_into(buf).await?),
            _ => panic!("Unknown player info action id: {}", id),
        })
    }
}
impl McBufWritable for Action {
    fn write_into(&self, buf: &mut Vec<u8>) -> Result<(), std::io::Error> {
        buf.write_byte(match self {
            Action::AddPlayer(_) => 0,
            Action::UpdateGameMode(_) => 1,
            Action::UpdateLatency(_) => 2,
            Action::UpdateDisplayName(_) => 3,
            Action::RemovePlayer(_) => 4,
        })?;
        match self {
            Action::AddPlayer(players) => players.write_into(buf)?,
            Action::UpdateGameMode(players) => players.write_into(buf)?,
            Action::UpdateLatency(players) => players.write_into(buf)?,
            Action::UpdateDisplayName(players) => players.write_into(buf)?,
            Action::RemovePlayer(players) => players.write_into(buf)?,
        }
        Ok(())
    }
}
