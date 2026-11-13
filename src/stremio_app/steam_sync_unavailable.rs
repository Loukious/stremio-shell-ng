use crate::stremio_app::sync_protocol::{MediaIdentity, SourceMode};
use flume::Sender;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum SyncUiEvent {
    HostStarted,
    JoinedHost,
    LobbyUpdated { member_count: i32, max_size: i32 },
    GuestJoined { name: String, member_count: i32 },
    GuestLeft { name: String, member_count: i32 },
    HostLeft { reason: String },
    LeftLobby,
    Error { message: String },
}

pub fn set_ui_event_sender(_sender: Sender<SyncUiEvent>) {}

pub fn leave_lobby() -> Result<(), String> {
    Ok(())
}

pub fn kick_member(_steam_id: u64) -> Result<(), String> {
    Err(unavailable_message())
}

pub fn start_host_lobby(
    _party_id: String,
    _max_size: i32,
    _source_mode: SourceMode,
) -> Result<String, String> {
    Err(unavailable_message())
}

pub fn update_web_media(
    _media: Option<MediaIdentity>,
    _selection_id: String,
    _generation: Option<u64>,
    _request_id: Option<String>,
    _load_intent: bool,
) {
}
pub fn notify_player_load_started(_file_epoch: u64) {}
pub fn notify_player_loaded(_file_epoch: u64) {}
pub fn notify_player_unloaded(_file_epoch: u64) {}

pub fn connect_to_host(_join_secret: &str) -> Result<(), String> {
    Err(unavailable_message())
}

fn unavailable_message() -> String {
    "Steam watch parties are not available in this build.".to_string()
}
