use crate::stremio_app::app::{
    LobbyRole, LOBBY_JOIN_SECRET, LOBBY_MAX_SIZE, LOBBY_MEMBERS, LOBBY_MEMBER_COUNT,
    LOBBY_PARTY_ID, LOBBY_ROLE,
};
use crate::stremio_app::stremio_player::player::{
    CURRENT_STREAM_URL, CURRENT_TIME, IS_PAUSED, PLAYER_CMD_TX, TOTAL_DURATION,
};
use crate::stremio_app::sync_protocol::{
    decode_steam_join_secret, encode_steam_join_secret, GuestMsg, HostMsg, LobbyMember,
    SteamJoinSecret, SteamSyncWireMsg, STEAM_SYNC_APP_ID, STEAM_SYNC_CHANNEL,
    STEAM_SYNC_PROTOCOL_VERSION,
};

use flume::{Receiver, Sender};
use once_cell::sync::Lazy;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
struct HostState {
    stream_url: String,
    web_url: String,
    time_pos: f64,
    duration: f64,
    paused: bool,
}

impl HostState {
    fn to_msg(&self) -> HostMsg {
        HostMsg::State {
            stream_url: self.stream_url.clone(),
            web_url: self.web_url.clone(),
            time_pos: self.time_pos,
            duration: self.duration,
            paused: self.paused,
        }
    }
}

#[derive(Debug)]
enum RuntimeCommand {
    StartHost {
        party_id: String,
        token: String,
        max_size: i32,
        reply: Sender<Result<SteamJoinSecret, String>>,
    },
    JoinHost {
        secret: SteamJoinSecret,
        reply: Sender<Result<(), String>>,
    },
    Kick {
        steam_id: u64,
    },
    Leave,
}

#[derive(Debug)]
enum RuntimeEvent {
    HostLobbyCreated {
        result: Result<steamworks::LobbyId, String>,
        party_id: String,
        token: String,
        max_size: i32,
        reply: Sender<Result<SteamJoinSecret, String>>,
    },
    GuestLobbyJoined {
        result: Result<steamworks::LobbyId, String>,
        secret: SteamJoinSecret,
        reply: Sender<Result<(), String>>,
    },
    LobbyChatUpdate(steamworks::LobbyChatUpdate),
    SessionFailed(Option<u64>),
}

#[derive(Debug)]
enum Mode {
    Host {
        lobby_id: steamworks::LobbyId,
        token: String,
        max_size: i32,
        authorized_peers: HashMap<u64, LobbyMember>,
        kicked_peers: HashSet<u64>,
        last_stream_url: String,
        last_web_url: String,
        last_paused: bool,
        last_time: f64,
        last_broadcast: Instant,
    },
    Guest {
        lobby_id: steamworks::LobbyId,
        host_steam_id: u64,
        current_web_url: String,
    },
}

#[derive(Debug, Clone, Copy)]
struct SessionPolicy {
    role: LobbyRole,
    host_steam_id: Option<u64>,
}

impl Default for SessionPolicy {
    fn default() -> Self {
        Self {
            role: LobbyRole::None,
            host_steam_id: None,
        }
    }
}

impl SessionPolicy {
    fn should_accept(&self, peer_id: u64) -> bool {
        match self.role {
            LobbyRole::Host => true,
            LobbyRole::Guest => self.host_steam_id == Some(peer_id),
            LobbyRole::None => false,
        }
    }
}

static RUNTIME_TX: Lazy<Mutex<Option<Sender<RuntimeCommand>>>> = Lazy::new(|| Mutex::new(None));
static UI_EVENT_TX: Lazy<Mutex<Option<Sender<SyncUiEvent>>>> = Lazy::new(|| Mutex::new(None));

pub fn set_ui_event_sender(sender: Sender<SyncUiEvent>) {
    if let Ok(mut tx) = UI_EVENT_TX.lock() {
        *tx = Some(sender);
    }
}

pub fn leave_lobby() -> Result<(), String> {
    let tx = ensure_runtime()?;
    tx.send(RuntimeCommand::Leave)
        .map_err(|e| format!("Steam sync runtime is not available: {e}"))
}

pub fn kick_member(steam_id: u64) -> Result<(), String> {
    let tx = ensure_runtime()?;
    tx.send(RuntimeCommand::Kick { steam_id })
        .map_err(|e| format!("Steam sync runtime is not available: {e}"))
}

pub fn start_host_lobby(party_id: String, max_size: i32) -> Result<String, String> {
    let token = generate_token();
    let tx = ensure_runtime()?;
    let (reply_tx, reply_rx) = flume::bounded(1);

    tx.send(RuntimeCommand::StartHost {
        party_id,
        token,
        max_size,
        reply: reply_tx,
    })
    .map_err(|e| format!("Steam sync runtime is not available: {e}"))?;

    let secret = reply_rx
        .recv_timeout(Duration::from_secs(20))
        .map_err(|e| format!("timed out creating Steam lobby: {e}"))??;

    encode_steam_join_secret(&secret)
}

pub fn connect_to_host(join_secret: &str) -> Result<(), String> {
    let secret = decode_steam_join_secret(join_secret)?;
    let tx = ensure_runtime()?;

    set_lobby_identity(&secret.party_id, join_secret);
    set_lobby_counts(1, 0);
    set_lobby_members(Vec::new());
    set_lobby_role(LobbyRole::Guest);

    let (reply_tx, reply_rx) = flume::bounded(1);

    tx.send(RuntimeCommand::JoinHost {
        secret,
        reply: reply_tx,
    })
    .map_err(|e| format!("Steam sync runtime is not available: {e}"))?;

    reply_rx
        .recv_timeout(Duration::from_secs(20))
        .map_err(|e| format!("timed out joining Steam lobby: {e}"))?
}

fn set_lobby_identity(party_id: &str, join_secret: &str) {
    if let Ok(mut id) = LOBBY_PARTY_ID.lock() {
        *id = party_id.to_string();
    }
    if let Ok(mut secret) = LOBBY_JOIN_SECRET.lock() {
        *secret = join_secret.to_string();
    }
}

fn ensure_runtime() -> Result<Sender<RuntimeCommand>, String> {
    let mut guard = RUNTIME_TX
        .lock()
        .map_err(|_| "Steam sync runtime lock is poisoned".to_string())?;
    if let Some(tx) = guard.as_ref() {
        return Ok(tx.clone());
    }

    let (tx, rx) = flume::unbounded();
    let (init_tx, init_rx) = flume::bounded(1);
    thread::Builder::new()
        .name("steam-sync-runtime".to_string())
        .spawn(move || run_runtime(rx, init_tx))
        .map_err(|e| format!("failed to spawn Steam sync runtime: {e}"))?;

    match init_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(e),
        Err(e) => return Err(format!("timed out initializing Steam sync runtime: {e}")),
    }

    *guard = Some(tx.clone());

    Ok(tx)
}

fn run_runtime(rx: Receiver<RuntimeCommand>, init_tx: Sender<Result<(), String>>) {
    let client = match steamworks::Client::init_app(STEAM_SYNC_APP_ID) {
        Ok(client) => client,
        Err(e) => {
            let message = steam_init_error_message(&e);
            eprintln!("Steam sync unavailable: {message}");
            emit_ui_event(SyncUiEvent::Error {
                message: message.clone(),
            });
            let _ = init_tx.send(Err(message));
            return;
        }
    };
    let _ = init_tx.send(Ok(()));

    client.networking_utils().init_relay_network_access();

    let (event_tx, event_rx) = flume::unbounded();
    let session_policy = Arc::new(Mutex::new(SessionPolicy::default()));
    let session_policy_requests = session_policy.clone();
    let messages = client.networking_messages();
    messages.session_request_callback(move |request| {
        let peer_id = request.remote().steam_id().map(|id| id.raw());
        let should_accept = peer_id
            .and_then(|id| {
                session_policy_requests
                    .lock()
                    .ok()
                    .map(|policy| policy.should_accept(id))
            })
            .unwrap_or(false);

        if should_accept {
            request.accept();
        } else {
            request.reject();
        }
    });
    let event_tx_session = event_tx.clone();
    messages.session_failed_callback(move |info| {
        let peer_id = info
            .identity_remote()
            .and_then(|identity| identity.steam_id())
            .map(|id| id.raw());
        let _ = event_tx_session.send(RuntimeEvent::SessionFailed(peer_id));
        eprintln!("Steam sync session failed: {info:?}");
    });

    let mut mode: Option<Mode> = None;

    loop {
        for command in rx.drain() {
            handle_command(&client, &event_tx, command, &mut mode);
        }

        update_session_policy(&session_policy, &mode);

        let mut callback_events = Vec::new();
        client.process_callbacks(|event| {
            if let steamworks::CallbackResult::LobbyChatUpdate(update) = event {
                callback_events.push(RuntimeEvent::LobbyChatUpdate(update));
            }
        });
        for event in callback_events {
            handle_event(&client, event, &mut mode);
        }

        for event in event_rx.drain() {
            handle_event(&client, event, &mut mode);
        }

        drain_incoming_messages(&client, &mut mode);
        tick_host_broadcast(&client, &mut mode);
        update_session_policy(&session_policy, &mode);

        thread::sleep(Duration::from_millis(30));
    }
}

fn steam_init_error_message(error: &steamworks::SteamAPIInitError) -> String {
    match error {
        steamworks::SteamAPIInitError::NoSteamClient(_) => {
            "Steam is not running. Open Steam, sign in, then try starting the watch party again."
                .to_string()
        }
        steamworks::SteamAPIInitError::VersionMismatch(_) => {
            "Steam needs to update before watch parties can use Steam networking. Update or restart Steam, then try again."
                .to_string()
        }
        steamworks::SteamAPIInitError::FailedGeneric(_) => {
            "Steam networking could not be initialized. Make sure Steam is open and signed in, then try again."
                .to_string()
        }
    }
}

fn update_session_policy(policy: &Arc<Mutex<SessionPolicy>>, mode: &Option<Mode>) {
    let next = match mode {
        Some(Mode::Host { .. }) => SessionPolicy {
            role: LobbyRole::Host,
            host_steam_id: None,
        },
        Some(Mode::Guest { host_steam_id, .. }) => SessionPolicy {
            role: LobbyRole::Guest,
            host_steam_id: Some(*host_steam_id),
        },
        None => SessionPolicy::default(),
    };

    if let Ok(mut current) = policy.lock() {
        *current = next;
    }
}

fn handle_command(
    client: &steamworks::Client,
    event_tx: &Sender<RuntimeEvent>,
    command: RuntimeCommand,
    mode: &mut Option<Mode>,
) {
    match command {
        RuntimeCommand::StartHost {
            party_id,
            token,
            max_size,
            reply,
        } => {
            leave_current_lobby(client, mode.take());
            let event_tx = event_tx.clone();
            client.matchmaking().create_lobby(
                steamworks::LobbyType::Invisible,
                max_size.max(2) as u32,
                move |result| {
                    let result = result.map_err(|e| format!("{e:?}"));
                    let _ = event_tx.send(RuntimeEvent::HostLobbyCreated {
                        result,
                        party_id,
                        token,
                        max_size,
                        reply,
                    });
                },
            );
        }
        RuntimeCommand::JoinHost { secret, reply } => {
            leave_current_lobby(client, mode.take());
            let event_tx = event_tx.clone();
            let lobby_id = steamworks::LobbyId::from_raw(secret.lobby_id);
            client.matchmaking().join_lobby(lobby_id, move |result| {
                let result = result.map_err(|_| "Steam lobby join failed".to_string());
                let _ = event_tx.send(RuntimeEvent::GuestLobbyJoined {
                    result,
                    secret,
                    reply,
                });
            });
        }
        RuntimeCommand::Leave => {
            leave_current_lobby(client, mode.take());
            emit_ui_event(SyncUiEvent::LeftLobby);
        }
        RuntimeCommand::Kick { steam_id } => {
            kick_authorized_peer(client, mode, steam_id);
        }
    }
}

fn handle_event(client: &steamworks::Client, event: RuntimeEvent, mode: &mut Option<Mode>) {
    match event {
        RuntimeEvent::HostLobbyCreated {
            result,
            party_id,
            token,
            max_size,
            reply,
        } => match result {
            Ok(lobby_id) => {
                let host_id = client.user().steam_id().raw();
                let matchmaking = client.matchmaking();
                matchmaking.set_lobby_data(lobby_id, "stremio_sync", "1");
                matchmaking.set_lobby_data(lobby_id, "protocol", "1");
                matchmaking.set_lobby_data(lobby_id, "app_id", &STEAM_SYNC_APP_ID.to_string());
                matchmaking.set_lobby_data(lobby_id, "host_steam_id", &host_id.to_string());
                matchmaking.set_lobby_data(lobby_id, "party_id", &party_id);
                matchmaking.set_lobby_joinable(lobby_id, true);

                *mode = Some(Mode::Host {
                    lobby_id,
                    token: token.clone(),
                    max_size,
                    authorized_peers: HashMap::new(),
                    kicked_peers: HashSet::new(),
                    last_stream_url: String::new(),
                    last_web_url: String::new(),
                    last_paused: false,
                    last_time: 0.0,
                    last_broadcast: Instant::now() - Duration::from_secs(1),
                });

                set_lobby_counts(1, max_size);
                set_lobby_members(host_lobby_members(client, &HashMap::new()));
                set_lobby_role(LobbyRole::Host);
                emit_ui_event(SyncUiEvent::HostStarted);

                let secret = SteamJoinSecret {
                    app_id: STEAM_SYNC_APP_ID,
                    lobby_id: lobby_id.raw(),
                    host_steam_id: host_id,
                    party_id,
                    token,
                };
                let _ = reply.send(Ok(secret));
            }
            Err(e) => {
                set_lobby_counts(0, max_size);
                let message = format!("failed to create Steam lobby: {e}");
                emit_ui_event(SyncUiEvent::Error {
                    message: message.clone(),
                });
                let _ = reply.send(Err(message));
            }
        },
        RuntimeEvent::GuestLobbyJoined {
            result,
            secret,
            reply,
        } => match result {
            Ok(lobby_id) => {
                *mode = Some(Mode::Guest {
                    lobby_id,
                    host_steam_id: secret.host_steam_id,
                    current_web_url: String::new(),
                });

                let hello = GuestMsg::Hello {
                    protocol_version: STEAM_SYNC_PROTOCOL_VERSION,
                    token: secret.token,
                    name: whoami::username(),
                };
                send_guest_msg(client, secret.host_steam_id, hello, true);
                send_guest_msg(client, secret.host_steam_id, GuestMsg::RequestState, true);
                set_lobby_role(LobbyRole::Guest);
                emit_ui_event(SyncUiEvent::JoinedHost);
                let _ = reply.send(Ok(()));
            }
            Err(e) => {
                clear_lobby_globals();
                let message = format!("failed to join Steam lobby: {e}");
                emit_ui_event(SyncUiEvent::Error {
                    message: message.clone(),
                });
                let _ = reply.send(Err(message));
            }
        },
        RuntimeEvent::LobbyChatUpdate(update) => {
            handle_lobby_chat_update(client, update, mode);
        }
        RuntimeEvent::SessionFailed(peer_id) => {
            if let Some(peer_id) = peer_id {
                handle_peer_left(client, mode, peer_id, "disconnected");
            }
        }
    }
}

fn drain_incoming_messages(client: &steamworks::Client, mode: &mut Option<Mode>) {
    let messages = client.networking_messages();
    for msg in messages.receive_messages_on_channel(STEAM_SYNC_CHANNEL, 32) {
        let peer_id = match msg.identity_peer().steam_id() {
            Some(id) => id.raw(),
            None => continue,
        };
        let wire_msg = match serde_json::from_slice::<SteamSyncWireMsg>(msg.data()) {
            Ok(msg) => msg,
            Err(e) => {
                eprintln!("Steam sync: failed to parse message from {peer_id}: {e}");
                continue;
            }
        };

        match mode {
            Some(Mode::Host { .. }) => handle_host_incoming(client, mode, peer_id, wire_msg),
            Some(Mode::Guest {
                current_web_url, ..
            }) => match wire_msg {
                SteamSyncWireMsg::Host(HostMsg::LobbyClosed { reason }) => {
                    leave_current_lobby(client, mode.take());
                    emit_ui_event(SyncUiEvent::HostLeft { reason });
                }
                SteamSyncWireMsg::Host(msg) => handle_host_message(msg, current_web_url),
                SteamSyncWireMsg::Error { message } => {
                    eprintln!("Steam sync host rejected request: {message}");
                }
                SteamSyncWireMsg::Guest(_) => {}
            },
            None => {}
        }
    }
}

fn handle_host_incoming(
    client: &steamworks::Client,
    mode: &mut Option<Mode>,
    peer_id: u64,
    wire_msg: SteamSyncWireMsg,
) {
    let (token, max_size, authorized_peers, kicked_peers) = match mode {
        Some(Mode::Host {
            token,
            max_size,
            authorized_peers,
            kicked_peers,
            ..
        }) => (token.clone(), *max_size, authorized_peers, kicked_peers),
        _ => return,
    };

    match wire_msg {
        SteamSyncWireMsg::Guest(GuestMsg::Hello {
            protocol_version,
            token: guest_token,
            name,
        }) => {
            if kicked_peers.contains(&peer_id) {
                send_error(client, peer_id, "you were removed from this watch party");
                return;
            }

            if protocol_version != STEAM_SYNC_PROTOCOL_VERSION || guest_token != token {
                send_error(client, peer_id, "invalid watch-party token");
                return;
            }

            if authorized_peers.len() + 1 >= max_size as usize {
                send_error(client, peer_id, "watch-party lobby is full");
                return;
            }

            let display_name = steam_display_name(client, peer_id, &name);
            authorized_peers.insert(
                peer_id,
                LobbyMember {
                    steam_id: peer_id,
                    name: display_name.clone(),
                    is_host: false,
                },
            );
            let members = host_lobby_members(client, authorized_peers);
            let member_count = members.len() as i32;
            let peers: Vec<u64> = authorized_peers.keys().copied().collect();
            set_lobby_counts(member_count, max_size);
            set_lobby_members(members.clone());
            emit_ui_event(SyncUiEvent::GuestJoined {
                name: display_name.clone(),
                member_count,
            });

            println!("Steam sync guest joined: {display_name} ({peer_id})");
            for peer in peers {
                send_host_msg(
                    client,
                    peer,
                    HostMsg::LobbyInfo {
                        member_count,
                        max_size,
                        members: members.clone(),
                    },
                    true,
                );
            }
            send_host_msg(client, peer_id, gather_host_state().to_msg(), true);
        }
        SteamSyncWireMsg::Guest(GuestMsg::RequestState) => {
            if authorized_peers.contains_key(&peer_id) {
                let members = host_lobby_members(client, authorized_peers);
                send_host_msg(
                    client,
                    peer_id,
                    HostMsg::LobbyInfo {
                        member_count: members.len() as i32,
                        max_size,
                        members,
                    },
                    true,
                );
                send_host_msg(client, peer_id, gather_host_state().to_msg(), true);
            }
        }
        SteamSyncWireMsg::Guest(GuestMsg::Leave) => {
            handle_peer_left(client, mode, peer_id, "left");
        }
        SteamSyncWireMsg::Host(_) | SteamSyncWireMsg::Error { .. } => {}
    }
}

fn handle_lobby_chat_update(
    client: &steamworks::Client,
    update: steamworks::LobbyChatUpdate,
    mode: &mut Option<Mode>,
) {
    match mode {
        Some(Mode::Host { lobby_id, .. }) if *lobby_id == update.lobby => {
            let peer_id = update.user_changed.raw();
            match update.member_state_change {
                steamworks::ChatMemberStateChange::Left => {
                    handle_peer_left(client, mode, peer_id, "left")
                }
                steamworks::ChatMemberStateChange::Disconnected => {
                    handle_peer_left(client, mode, peer_id, "disconnected")
                }
                steamworks::ChatMemberStateChange::Kicked => {
                    handle_peer_left(client, mode, peer_id, "was removed")
                }
                steamworks::ChatMemberStateChange::Banned => {
                    handle_peer_left(client, mode, peer_id, "was removed")
                }
                steamworks::ChatMemberStateChange::Entered => {}
            }
        }
        Some(Mode::Guest {
            lobby_id,
            host_steam_id,
            ..
        }) if *lobby_id == update.lobby && update.user_changed.raw() == *host_steam_id => {
            match update.member_state_change {
                steamworks::ChatMemberStateChange::Left
                | steamworks::ChatMemberStateChange::Disconnected
                | steamworks::ChatMemberStateChange::Kicked
                | steamworks::ChatMemberStateChange::Banned => {
                    let reason = "The host left the watch party".to_string();
                    leave_current_lobby(client, mode.take());
                    emit_ui_event(SyncUiEvent::HostLeft { reason });
                }
                steamworks::ChatMemberStateChange::Entered => {}
            }
        }
        _ => {}
    }
}

fn handle_peer_left(
    client: &steamworks::Client,
    mode: &mut Option<Mode>,
    peer_id: u64,
    reason: &str,
) {
    let (name, member_count, max_size, members, peers) = match mode {
        Some(Mode::Host {
            authorized_peers,
            max_size,
            ..
        }) => {
            let Some(member) = authorized_peers.remove(&peer_id) else {
                return;
            };
            let members = host_lobby_members(client, authorized_peers);
            let member_count = members.len() as i32;
            let peers: Vec<u64> = authorized_peers.keys().copied().collect();
            (member.name, member_count, *max_size, members, peers)
        }
        _ => return,
    };

    set_lobby_counts(member_count, max_size);
    set_lobby_members(members.clone());
    emit_ui_event(SyncUiEvent::GuestLeft {
        name: name.clone(),
        member_count,
    });
    println!("Steam sync guest {reason}: {name} ({peer_id})");

    for peer in peers {
        send_host_msg(
            client,
            peer,
            HostMsg::LobbyInfo {
                member_count,
                max_size,
                members: members.clone(),
            },
            true,
        );
    }
}

fn kick_authorized_peer(client: &steamworks::Client, mode: &mut Option<Mode>, peer_id: u64) {
    let (name, member_count, max_size, members, peers) = match mode {
        Some(Mode::Host {
            authorized_peers,
            kicked_peers,
            max_size,
            ..
        }) => {
            let Some(member) = authorized_peers.remove(&peer_id) else {
                return;
            };
            kicked_peers.insert(peer_id);
            let members = host_lobby_members(client, authorized_peers);
            let member_count = members.len() as i32;
            let peers: Vec<u64> = authorized_peers.keys().copied().collect();
            (member.name, member_count, *max_size, members, peers)
        }
        _ => return,
    };

    send_host_msg(
        client,
        peer_id,
        HostMsg::LobbyClosed {
            reason: "You were removed from the watch party".to_string(),
        },
        true,
    );

    set_lobby_counts(member_count, max_size);
    set_lobby_members(members.clone());
    emit_ui_event(SyncUiEvent::GuestLeft {
        name: name.clone(),
        member_count,
    });
    println!("Steam sync guest removed: {name} ({peer_id})");

    for peer in peers {
        send_host_msg(
            client,
            peer,
            HostMsg::LobbyInfo {
                member_count,
                max_size,
                members: members.clone(),
            },
            true,
        );
    }
}

fn host_lobby_members(
    client: &steamworks::Client,
    authorized_peers: &HashMap<u64, LobbyMember>,
) -> Vec<LobbyMember> {
    let mut members = vec![LobbyMember {
        steam_id: client.user().steam_id().raw(),
        name: clean_display_name(&client.friends().name(), "Host"),
        is_host: true,
    }];
    members.extend(authorized_peers.values().cloned());
    members
}

fn steam_display_name(client: &steamworks::Client, steam_id: u64, fallback: &str) -> String {
    let friends = client.friends();
    let steam_id = steamworks::SteamId::from_raw(steam_id);
    friends.request_user_information(steam_id, true);
    let friend = friends.get_friend(steam_id);
    clean_display_name(&friend.name(), fallback)
}

fn clean_display_name(name: &str, fallback: &str) -> String {
    let cleaned = name.trim();
    let cleaned = if cleaned.is_empty() {
        fallback
    } else {
        cleaned
    };
    cleaned.chars().take(48).collect()
}

fn tick_host_broadcast(client: &steamworks::Client, mode: &mut Option<Mode>) {
    let (authorized_peers, last_stream_url, last_web_url, last_paused, last_time, last_broadcast) =
        match mode {
            Some(Mode::Host {
                authorized_peers,
                last_stream_url,
                last_web_url,
                last_paused,
                last_time,
                last_broadcast,
                ..
            }) => (
                authorized_peers.keys().copied().collect::<Vec<_>>(),
                last_stream_url,
                last_web_url,
                last_paused,
                last_time,
                last_broadcast,
            ),
            _ => return,
        };

    if authorized_peers.is_empty() || last_broadcast.elapsed() < Duration::from_secs(1) {
        return;
    }

    let state = gather_host_state();
    let mut reliable_messages = Vec::new();

    if (state.stream_url != *last_stream_url && !state.stream_url.is_empty())
        || (state.web_url != *last_web_url && !state.web_url.is_empty())
    {
        reliable_messages.push(HostMsg::Load {
            stream_url: state.stream_url.clone(),
            web_url: state.web_url.clone(),
        });
        *last_stream_url = state.stream_url.clone();
        *last_web_url = state.web_url.clone();
    }

    if state.paused != *last_paused {
        if state.paused {
            reliable_messages.push(HostMsg::Pause {
                time_pos: state.time_pos,
            });
        } else {
            reliable_messages.push(HostMsg::Play {
                time_pos: state.time_pos,
            });
        }
        *last_paused = state.paused;
    }

    let expected_time = if *last_paused {
        *last_time
    } else {
        *last_time + 1.0
    };
    if (state.time_pos - expected_time).abs() > 3.0 {
        reliable_messages.push(HostMsg::Seek {
            time_pos: state.time_pos,
        });
    }
    *last_time = state.time_pos;
    *last_broadcast = Instant::now();

    for peer_id in authorized_peers {
        for msg in &reliable_messages {
            send_host_msg(client, peer_id, msg.clone(), true);
        }
        send_host_msg(client, peer_id, state.to_msg(), false);
    }
}

fn gather_host_state() -> HostState {
    let stream_url = CURRENT_STREAM_URL
        .lock()
        .map(|s| s.clone())
        .unwrap_or_default();
    let web_url = crate::stremio_app::stremio_wevbiew::wevbiew::CURRENT_URL
        .lock()
        .map(|s| s.clone())
        .unwrap_or_default();
    let time_pos = CURRENT_TIME.lock().map(|t| *t).unwrap_or(0.0);
    let duration = TOTAL_DURATION.lock().map(|d| *d).unwrap_or(0.0);
    let paused = IS_PAUSED.lock().map(|p| *p).unwrap_or(false);

    HostState {
        stream_url,
        web_url,
        time_pos,
        duration,
        paused,
    }
}

fn handle_host_message(msg: HostMsg, current_web_url: &mut String) {
    match msg {
        HostMsg::State {
            web_url,
            time_pos,
            paused,
            ..
        } => {
            if !web_url.is_empty() && web_url != *current_web_url {
                if let Some(hash_part) = web_url.split_once("#/").map(|(_, p)| p) {
                    let cmd_url = format!("stremio:///{}", hash_part);
                    if let Ok(tx_guard) =
                        crate::stremio_app::stremio_wevbiew::wevbiew::WEB_CMD_TX.lock()
                    {
                        if let Some(tx) = tx_guard.as_ref() {
                            tx.send(crate::stremio_app::ipc::RPCResponse::open_media(cmd_url))
                                .ok();
                        }
                    }
                }
                *current_web_url = web_url;
                thread::sleep(Duration::from_millis(1500));
            }

            send_player_cmd(&format!(r#"["mpv-set-prop",["pause",{}]]"#, paused));

            let local_time = CURRENT_TIME.lock().map(|t| *t).unwrap_or(0.0);
            let local_duration = TOTAL_DURATION.lock().map(|t| *t).unwrap_or(0.0);

            if local_duration > 0.0 && (local_time - time_pos).abs() > 2.0 {
                send_player_cmd(&format!(
                    r#"["mpv-command",["seek","{}","absolute"]]"#,
                    time_pos
                ));
            }
        }
        HostMsg::Load { stream_url, .. } => {
            println!("Steam sync: loading stream {stream_url}");
        }
        HostMsg::Play { time_pos } => {
            send_player_cmd(r#"["mpv-set-prop",["pause",false]]"#);
            send_player_cmd(&format!(
                r#"["mpv-command",["seek","{}","absolute"]]"#,
                time_pos
            ));
        }
        HostMsg::Pause { time_pos } => {
            send_player_cmd(r#"["mpv-set-prop",["pause",true]]"#);
            send_player_cmd(&format!(
                r#"["mpv-command",["seek","{}","absolute"]]"#,
                time_pos
            ));
        }
        HostMsg::Seek { time_pos } => {
            send_player_cmd(&format!(
                r#"["mpv-command",["seek","{}","absolute"]]"#,
                time_pos
            ));
        }
        HostMsg::LobbyInfo {
            member_count,
            max_size,
            members,
        } => {
            set_lobby_counts(member_count, max_size);
            set_lobby_members(members);
            emit_ui_event(SyncUiEvent::LobbyUpdated {
                member_count,
                max_size,
            });
        }
        HostMsg::LobbyClosed { .. } => {}
    }
}

fn send_guest_msg(client: &steamworks::Client, peer_id: u64, msg: GuestMsg, reliable: bool) {
    send_wire_msg(
        client,
        peer_id,
        SteamSyncWireMsg::Guest(msg),
        if reliable {
            steamworks::networking_types::SendFlags::RELIABLE
        } else {
            steamworks::networking_types::SendFlags::UNRELIABLE_NO_DELAY
        },
    );
}

fn send_host_msg(client: &steamworks::Client, peer_id: u64, msg: HostMsg, reliable: bool) {
    send_wire_msg(
        client,
        peer_id,
        SteamSyncWireMsg::Host(msg),
        if reliable {
            steamworks::networking_types::SendFlags::RELIABLE
        } else {
            steamworks::networking_types::SendFlags::UNRELIABLE_NO_DELAY
        },
    );
}

fn send_error(client: &steamworks::Client, peer_id: u64, message: &str) {
    send_wire_msg(
        client,
        peer_id,
        SteamSyncWireMsg::Error {
            message: message.to_string(),
        },
        steamworks::networking_types::SendFlags::RELIABLE,
    );
}

fn send_wire_msg(
    client: &steamworks::Client,
    peer_id: u64,
    msg: SteamSyncWireMsg,
    flags: steamworks::networking_types::SendFlags,
) {
    let data = match serde_json::to_vec(&msg) {
        Ok(data) => data,
        Err(e) => {
            eprintln!("Steam sync: failed to serialize message: {e}");
            return;
        }
    };

    let identity = steamworks::networking_types::NetworkingIdentity::new_steam_id(
        steamworks::SteamId::from_raw(peer_id),
    );
    let flags = flags | steamworks::networking_types::SendFlags::AUTO_RESTART_BROKEN_SESSION;
    if let Err(e) = client.networking_messages().send_message_to_user(
        identity,
        flags,
        &data,
        STEAM_SYNC_CHANNEL,
    ) {
        eprintln!("Steam sync: failed to send message to {peer_id}: {e:?}");
    }
}

fn send_player_cmd(json: &str) {
    if let Ok(guard) = PLAYER_CMD_TX.lock() {
        if let Some(tx) = guard.as_ref() {
            if let Err(e) = tx.send(json.to_string()) {
                eprintln!("Steam sync: failed to send player command: {e}");
            }
        } else {
            eprintln!("Steam sync: player command sender not initialized");
        }
    }
}

fn leave_current_lobby(client: &steamworks::Client, mode: Option<Mode>) {
    match mode {
        Some(Mode::Host {
            lobby_id,
            authorized_peers,
            ..
        }) => {
            for peer_id in authorized_peers.keys().copied() {
                send_host_msg(
                    client,
                    peer_id,
                    HostMsg::LobbyClosed {
                        reason: "The host left the watch party".to_string(),
                    },
                    true,
                );
            }
            client.matchmaking().leave_lobby(lobby_id);
            clear_lobby_globals();
        }
        Some(Mode::Guest {
            lobby_id,
            host_steam_id,
            ..
        }) => {
            send_guest_msg(client, host_steam_id, GuestMsg::Leave, true);
            client.matchmaking().leave_lobby(lobby_id);
            clear_lobby_globals();
        }
        None => {}
    }
}

fn set_lobby_counts(member_count: i32, max_size: i32) {
    if let Ok(mut cnt) = LOBBY_MEMBER_COUNT.lock() {
        *cnt = member_count;
    }
    if max_size > 0 {
        if let Ok(mut max) = LOBBY_MAX_SIZE.lock() {
            *max = max_size;
        }
    }
}

fn set_lobby_members(members: Vec<LobbyMember>) {
    if let Ok(mut current_members) = LOBBY_MEMBERS.lock() {
        *current_members = members;
    }
}

fn set_lobby_role(role: LobbyRole) {
    if let Ok(mut current_role) = LOBBY_ROLE.lock() {
        *current_role = role;
    }
}

fn clear_lobby_globals() {
    if let Ok(mut id) = LOBBY_PARTY_ID.lock() {
        id.clear();
    }
    if let Ok(mut secret) = LOBBY_JOIN_SECRET.lock() {
        secret.clear();
    }
    set_lobby_counts(0, 0);
    set_lobby_members(Vec::new());
    set_lobby_role(LobbyRole::None);
}

fn emit_ui_event(event: SyncUiEvent) {
    if let Ok(tx) = UI_EVENT_TX.lock() {
        if let Some(tx) = tx.as_ref() {
            let _ = tx.send(event);
        }
    }
}

fn generate_token() -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};

    let mut bytes = [0u8; 32];
    rand::fill(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_token_gate_accepts_only_matching_token_and_version() {
        fn allowed(expected: &str, version: u16, token: &str) -> bool {
            version == STEAM_SYNC_PROTOCOL_VERSION && token == expected
        }

        assert!(allowed("secret", STEAM_SYNC_PROTOCOL_VERSION, "secret"));
        assert!(!allowed("secret", STEAM_SYNC_PROTOCOL_VERSION, "wrong"));
        assert!(!allowed(
            "secret",
            STEAM_SYNC_PROTOCOL_VERSION + 1,
            "secret"
        ));
    }

    #[test]
    fn generated_tokens_are_non_empty_and_distinct() {
        let a = generate_token();
        let b = generate_token();

        assert!(!a.is_empty());
        assert!(!b.is_empty());
        assert_ne!(a, b);
    }
}
