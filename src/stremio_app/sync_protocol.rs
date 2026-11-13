#![cfg_attr(not(feature = "steam-sync"), allow(dead_code))]

use serde::{Deserialize, Serialize};

pub const STEAM_SYNC_APP_ID: u32 = 480;
pub const STEAM_SYNC_PROTOCOL_VERSION: u16 = 2;
pub const STEAM_SYNC_CHANNEL: u32 = 4800;
pub const STEAM_JOIN_PREFIX: &str = "stremio://sync/steam/v2/";

#[derive(Serialize, Deserialize, Debug, Default, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceMode {
    #[default]
    Shared,
    Local,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MediaIdentity {
    pub content_type: String,
    pub metadata_id: String,
    pub video_id: String,
}

impl MediaIdentity {
    pub fn validate(&self) -> Result<(), String> {
        let valid_type = !self.content_type.is_empty()
            && self.content_type.len() <= 64
            && self
                .content_type
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'));
        if !valid_type {
            return Err("Watch-party content type is invalid.".to_string());
        }
        for (name, id) in [("metadata", &self.metadata_id), ("video", &self.video_id)] {
            let valid = !id.is_empty()
                && id.len() <= 256
                && id.bytes().any(|byte| byte.is_ascii_alphanumeric())
                && id.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'_' | b'-' | b'.')
                });
            let lower = id.to_ascii_lowercase();
            let is_url = ["http:", "https:", "file:", "stremio:", "magnet:", "data:"]
                .iter()
                .any(|scheme| lower.starts_with(scheme));
            if !valid || is_url {
                return Err(format!("Watch-party {name} ID is invalid."));
            }
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LocalPlaybackState {
    pub media: MediaIdentity,
    pub generation: u64,
    pub sequence: u64,
    pub time_pos: f64,
    pub duration: f64,
    pub paused: bool,
}

impl LocalPlaybackState {
    pub fn validate(&self) -> Result<(), String> {
        self.media.validate()?;
        if self.generation == 0 || self.sequence == 0 {
            return Err("Watch-party media generation or sequence is invalid.".to_string());
        }
        // This mode is for movies and episodes, not unbounded live timelines.
        const MAX_MEDIA_SECONDS: f64 = 31.0 * 24.0 * 60.0 * 60.0;
        if [self.time_pos, self.duration]
            .iter()
            .any(|value| !value.is_finite() || *value < 0.0 || *value > MAX_MEDIA_SECONDS)
        {
            return Err("Watch-party playback position or duration is invalid.".to_string());
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct LobbyMember {
    pub steam_id: u64,
    pub name: String,
    pub is_host: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type")]
pub enum HostMsg {
    #[serde(rename = "local_state")]
    LocalState { state: LocalPlaybackState },
    #[serde(rename = "local_waiting")]
    LocalWaiting { generation: u64, sequence: u64 },
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
        #[serde(default)]
        local_sources: bool,
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
    #[serde(default = "legacy_protocol_version")]
    pub protocol_version: u16,
    #[serde(default)]
    pub source_mode: SourceMode,
    pub app_id: u32,
    pub lobby_id: u64,
    pub host_steam_id: u64,
    pub party_id: String,
    pub token: String,
}

fn legacy_protocol_version() -> u16 {
    1
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

    if join_url.starts_with("stremio://sync/steam/") && !join_url.starts_with(STEAM_JOIN_PREFIX) {
        return Err("This watch-party link uses an incompatible protocol. Update both clients and create a new invitation.".to_string());
    }
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
    if secret.protocol_version != STEAM_SYNC_PROTOCOL_VERSION {
        return Err("This watch-party link uses an incompatible protocol. Update both clients and create a new invitation.".to_string());
    }
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
            protocol_version: STEAM_SYNC_PROTOCOL_VERSION,
            source_mode: SourceMode::Local,
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
    fn rejects_legacy_links_and_missing_or_wrong_protocol_versions() {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};

        assert!(decode_steam_join_secret("stremio://sync/steam/v1/anything")
            .unwrap_err()
            .contains("Update both clients"));
        let mut incompatible = sample_secret();
        incompatible.protocol_version = 1;
        let encoded = encode_steam_join_secret(&incompatible).unwrap();
        assert!(decode_steam_join_secret(&encoded)
            .unwrap_err()
            .contains("incompatible protocol"));
        let mut missing_version = serde_json::to_value(sample_secret()).unwrap();
        missing_version
            .as_object_mut()
            .unwrap()
            .remove("protocol_version");
        let encoded = format!(
            "{STEAM_JOIN_PREFIX}{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&missing_version).unwrap())
        );
        assert!(decode_steam_join_secret(&encoded)
            .unwrap_err()
            .contains("incompatible protocol"));
    }

    fn local_state() -> LocalPlaybackState {
        LocalPlaybackState {
            media: MediaIdentity {
                content_type: "series".to_string(),
                metadata_id: "tt1234567".to_string(),
                video_id: "tt1234567:1:2".to_string(),
            },
            generation: 3,
            sequence: 14,
            time_pos: 240.0,
            duration: 3200.0,
            paused: true,
        }
    }

    #[test]
    fn local_wire_state_has_only_identity_and_playback_fields() {
        let message = SteamSyncWireMsg::Host(HostMsg::LocalState {
            state: local_state(),
        });
        let json = serde_json::to_value(&message).unwrap();
        let state = json["payload"]["state"].as_object().unwrap();
        assert_eq!(state.len(), 6);
        assert_eq!(state["media"]["video_id"], "tt1234567:1:2");
        assert_eq!(state["media"].as_object().unwrap().len(), 3);
        let encoded = serde_json::to_string(&message).unwrap();
        for excluded in [
            "stream_url",
            "web_url",
            "addon",
            "headers",
            "deepLinks",
            "http",
        ] {
            assert!(
                !encoded.contains(excluded),
                "unexpected local field: {}",
                excluded
            );
        }
        assert_eq!(
            serde_json::from_str::<SteamSyncWireMsg>(&encoded).unwrap(),
            message
        );

        let mut extra = serde_json::to_value(local_state()).unwrap();
        extra["stream_url"] = serde_json::json!("https://private.example/video");
        assert!(serde_json::from_value::<LocalPlaybackState>(extra).is_err());
    }

    #[test]
    fn rejects_urls_malformed_identifiers_and_invalid_playback_values() {
        for id in [
            "https://private.example/video?token=secret",
            "https:secret",
            "tt1234567/1/2",
            "tt1234567?api_key=secret",
            "../encoded-player",
            "..",
            "::",
            "",
        ] {
            let mut state = local_state();
            state.media.video_id = id.to_string();
            assert!(state.validate().is_err(), "accepted {}", id);
        }
        for invalid in [f64::NAN, f64::INFINITY, -1.0, 1e100] {
            let mut state = local_state();
            state.time_pos = invalid;
            assert!(state.validate().is_err());
            state.time_pos = 0.0;
            state.duration = invalid;
            assert!(state.validate().is_err());
        }
        let mut missing_generation = local_state();
        missing_generation.generation = 0;
        assert!(missing_generation.validate().is_err());
        let mut missing_sequence = local_state();
        missing_sequence.sequence = 0;
        assert!(missing_sequence.validate().is_err());
        assert!(local_state().validate().is_ok());
    }

    #[test]
    fn absent_local_capability_is_false_and_cannot_be_assumed() {
        let json = r#"{"type":"hello","protocol_version":2,"token":"secret","name":"Guest"}"#;
        let hello = serde_json::from_str::<GuestMsg>(json).unwrap();
        assert!(matches!(
            hello,
            GuestMsg::Hello {
                local_sources: false,
                ..
            }
        ));
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
            local_sources: true,
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
