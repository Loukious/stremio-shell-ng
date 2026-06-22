#![cfg_attr(not(feature = "steam-sync"), allow(dead_code))]

use serde::{Deserialize, Serialize};

pub const STEAM_SYNC_APP_ID: u32 = 480;
pub const STEAM_SYNC_PROTOCOL_VERSION: u16 = 1;
pub const STEAM_SYNC_CHANNEL: u32 = 4800;
pub const STEAM_JOIN_PREFIX: &str = "stremio://sync/steam/v1/";

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct LobbyMember {
    pub steam_id: u64,
    pub name: String,
    pub is_host: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type")]
pub enum HostMsg {
    #[serde(rename = "state")]
    State {
        stream_url: String,
        web_url: String,
        time_pos: f64,
        duration: f64,
        paused: bool,
    },
    #[serde(rename = "play")]
    Play { time_pos: f64 },
    #[serde(rename = "pause")]
    Pause { time_pos: f64 },
    #[serde(rename = "seek")]
    Seek { time_pos: f64 },
    #[serde(rename = "load")]
    Load { stream_url: String, web_url: String },
    #[serde(rename = "lobby_info")]
    LobbyInfo {
        member_count: i32,
        max_size: i32,
        #[serde(default)]
        members: Vec<LobbyMember>,
    },
    #[serde(rename = "lobby_closed")]
    LobbyClosed { reason: String },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type")]
pub enum GuestMsg {
    #[serde(rename = "hello")]
    Hello {
        protocol_version: u16,
        token: String,
        name: String,
    },
    #[serde(rename = "request_state")]
    RequestState,
    #[serde(rename = "leave")]
    Leave,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind", content = "payload")]
pub enum SteamSyncWireMsg {
    Host(HostMsg),
    Guest(GuestMsg),
    Error { message: String },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SteamJoinSecret {
    pub app_id: u32,
    pub lobby_id: u64,
    pub host_steam_id: u64,
    pub party_id: String,
    pub token: String,
}

pub fn encode_steam_join_secret(secret: &SteamJoinSecret) -> Result<String, String> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};

    let json = serde_json::to_vec(secret)
        .map_err(|e| format!("failed to serialize Steam join secret: {e}"))?;
    Ok(format!(
        "{STEAM_JOIN_PREFIX}{}",
        URL_SAFE_NO_PAD.encode(json)
    ))
}

pub fn decode_steam_join_secret(join_url: &str) -> Result<SteamJoinSecret, String> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};

    let payload = join_url
        .strip_prefix(STEAM_JOIN_PREFIX)
        .ok_or_else(|| "not a Steam sync join URL".to_string())?;
    let decoded = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|e| format!("invalid Steam sync join payload: {e}"))?;
    let secret: SteamJoinSecret = serde_json::from_slice(&decoded)
        .map_err(|e| format!("invalid Steam sync join JSON: {e}"))?;

    validate_steam_join_secret(&secret)?;
    Ok(secret)
}

pub fn validate_steam_join_secret(secret: &SteamJoinSecret) -> Result<(), String> {
    if secret.app_id != STEAM_SYNC_APP_ID {
        return Err(format!(
            "unsupported Steam app id {}; expected {STEAM_SYNC_APP_ID}",
            secret.app_id
        ));
    }
    if secret.lobby_id == 0 {
        return Err("Steam lobby id is missing".to_string());
    }
    if secret.host_steam_id == 0 {
        return Err("host Steam id is missing".to_string());
    }
    if secret.party_id.trim().is_empty() {
        return Err("party id is missing".to_string());
    }
    if secret.token.trim().is_empty() {
        return Err("join token is missing".to_string());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_secret() -> SteamJoinSecret {
        SteamJoinSecret {
            app_id: STEAM_SYNC_APP_ID,
            lobby_id: 12,
            host_steam_id: 34,
            party_id: "party".to_string(),
            token: "token".to_string(),
        }
    }

    #[test]
    fn steam_secret_round_trips() {
        let secret = sample_secret();
        let encoded = encode_steam_join_secret(&secret).unwrap();

        assert!(encoded.starts_with(STEAM_JOIN_PREFIX));
        assert_eq!(decode_steam_join_secret(&encoded).unwrap(), secret);
    }

    #[test]
    fn rejects_bad_steam_secret_inputs() {
        assert!(decode_steam_join_secret("stremio://sync/127.0.0.1:1234").is_err());

        let mut wrong_app = sample_secret();
        wrong_app.app_id = 481;
        let encoded = encode_steam_join_secret(&wrong_app).unwrap();
        assert!(decode_steam_join_secret(&encoded).is_err());

        let mut missing_token = sample_secret();
        missing_token.token.clear();
        let encoded = encode_steam_join_secret(&missing_token).unwrap();
        assert!(decode_steam_join_secret(&encoded).is_err());
    }

    #[test]
    fn sync_messages_serialize() {
        let host = SteamSyncWireMsg::Host(HostMsg::Pause { time_pos: 42.0 });
        let json = serde_json::to_string(&host).unwrap();
        assert_eq!(
            serde_json::from_str::<SteamSyncWireMsg>(&json).unwrap(),
            host
        );

        let closed = SteamSyncWireMsg::Host(HostMsg::LobbyClosed {
            reason: "host left".to_string(),
        });
        let json = serde_json::to_string(&closed).unwrap();
        assert_eq!(
            serde_json::from_str::<SteamSyncWireMsg>(&json).unwrap(),
            closed
        );

        let lobby = SteamSyncWireMsg::Host(HostMsg::LobbyInfo {
            member_count: 2,
            max_size: 8,
            members: vec![LobbyMember {
                steam_id: 34,
                name: "Host".to_string(),
                is_host: true,
            }],
        });
        let json = serde_json::to_string(&lobby).unwrap();
        assert_eq!(
            serde_json::from_str::<SteamSyncWireMsg>(&json).unwrap(),
            lobby
        );

        let guest = SteamSyncWireMsg::Guest(GuestMsg::Hello {
            protocol_version: STEAM_SYNC_PROTOCOL_VERSION,
            token: "abc".to_string(),
            name: "viewer".to_string(),
        });
        let json = serde_json::to_string(&guest).unwrap();
        assert_eq!(
            serde_json::from_str::<SteamSyncWireMsg>(&json).unwrap(),
            guest
        );

        let leave = SteamSyncWireMsg::Guest(GuestMsg::Leave);
        let json = serde_json::to_string(&leave).unwrap();
        assert_eq!(
            serde_json::from_str::<SteamSyncWireMsg>(&json).unwrap(),
            leave
        );
    }
}
