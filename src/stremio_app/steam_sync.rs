use crate::stremio_app::app::{
    LobbyRole, LOBBY_JOIN_SECRET, LOBBY_MAX_SIZE, LOBBY_MEMBERS, LOBBY_MEMBER_COUNT,
    LOBBY_PARTY_ID, LOBBY_ROLE,
};
use crate::stremio_app::stremio_player::player::{
    CURRENT_STREAM_URL, CURRENT_TIME, FILE_LOAD_EPOCH, IS_PAUSED, PLAYER_CMD_TX, TOTAL_DURATION,
};
use crate::stremio_app::sync_protocol::{
    decode_steam_join_secret, encode_steam_join_secret, GuestMsg, GuestReadiness, HostMsg,
    LobbyMember, LocalPlaybackState, MediaIdentity, PartyReadiness, ReadinessStatus, SourceMode,
    SteamJoinSecret, SteamSyncWireMsg, STEAM_SYNC_APP_ID, STEAM_SYNC_CHANNEL,
    STEAM_SYNC_PROTOCOL_VERSION,
};
use crate::stremio_app::watch_party_local::{
    current_local_sync_revision, invalidate_local_sync, GuestLocalPlayback, GuestStateChange,
    LocalSyncCommand,
};
use crate::stremio_app::watch_party_readiness::{GuestReadinessRoster, HostReadinessRoster};

use flume::{Receiver, Sender};
use once_cell::sync::Lazy;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub enum SyncUiEvent {
    HostStarted,
    JoinedHost,
    LobbyUpdated {
        member_count: i32,
        max_size: i32,
    },
    GuestJoined {
        name: String,
        member_count: i32,
    },
    GuestLeft {
        name: String,
        member_count: i32,
    },
    HostLeft {
        reason: String,
    },
    LeftLobby,
    ReadinessUpdated {
        announcement: Option<String>,
        media: Option<MediaIdentity>,
        generation: u64,
        sequence: u64,
    },
    Error {
        message: String,
    },
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
        source_mode: SourceMode,
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
    WebMediaChanged(WebMediaContext),
    PlayerLoading {
        file_epoch: u64,
        context: WebMediaContext,
    },
    PlayerLoaded {
        file_epoch: u64,
    },
    PlayerReady {
        file_epoch: u64,
    },
    PlayerUnloaded {
        file_epoch: u64,
    },
}

#[derive(Debug)]
enum RuntimeEvent {
    HostLobbyCreated {
        result: Result<steamworks::LobbyId, String>,
        party_id: String,
        token: String,
        max_size: i32,
        source_mode: SourceMode,
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
        source_mode: SourceMode,
        local_host: LocalHostPlayback,
        readiness: HostReadinessRoster,
        readiness_peers: HashSet<u64>,
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
        source_mode: SourceMode,
        local: GuestLocalPlayback,
        readiness: GuestReadinessUpdates,
        last_context: Option<WebMediaContext>,
        shared_wait_epoch: u64,
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct WebMediaContext {
    media: Option<MediaIdentity>,
    selection_id: String,
    generation: Option<u64>,
    request_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct NativeMediaSnapshot {
    context: WebMediaContext,
    file_epoch: u64,
    loaded: bool,
    loading: bool,
    playback_ready: bool,
}

#[derive(Debug, Default)]
struct GuestReadinessUpdates {
    supported: bool,
    sequence: u64,
    last_report: Option<GuestReadiness>,
    roster: GuestReadinessRoster,
}

impl GuestReadinessUpdates {
    fn report(
        &mut self,
        local: &GuestLocalPlayback,
        native: &NativeMediaSnapshot,
        current_epoch: u64,
    ) -> Option<GuestReadiness> {
        if !self.supported {
            return None;
        }
        let selection = local.selection()?;
        let bound =
            local.file_epoch() == Some(native.file_epoch) && native.file_epoch == current_epoch;
        let status = if bound && native.loaded && native.playback_ready && local.is_ready() {
            ReadinessStatus::Ready
        } else if bound && (native.loading || native.loaded) {
            ReadinessStatus::Loading
        } else {
            ReadinessStatus::Selecting
        };
        if self.last_report.as_ref().is_some_and(|previous| {
            previous.media == selection.media
                && previous.generation == selection.generation
                && previous.status == status
        }) {
            return None;
        }
        self.sequence = self.sequence.checked_add(1)?;
        let state = GuestReadiness {
            media: selection.media.clone(),
            generation: selection.generation,
            sequence: self.sequence,
            status,
        };
        self.last_report = Some(state.clone());
        Some(state)
    }
}

#[derive(Debug, Default)]
struct LocalHostPlayback {
    generation: u64,
    sequence: u64,
    media: Option<MediaIdentity>,
}

impl LocalHostPlayback {
    fn snapshot(
        &mut self,
        native: &NativeMediaSnapshot,
        state: &HostState,
        current_file_epoch: u64,
    ) -> Option<HostMsg> {
        self.sequence = self.sequence.checked_add(1)?;
        if !native.loaded
            || native.file_epoch != current_file_epoch
            || native.context.media.is_none()
        {
            return Some(HostMsg::LocalWaiting {
                generation: self.generation.max(1),
                sequence: self.sequence,
            });
        }
        let media = native.context.media.clone()?;
        // Guests choose independent sources, so a host source reload for the
        // same episode must preserve their selection and loaded-file binding.
        if self.media.as_ref() != Some(&media) {
            self.generation = self.generation.checked_add(1)?;
            self.media = Some(media.clone());
        }
        let state = LocalPlaybackState {
            media,
            generation: self.generation,
            sequence: self.sequence,
            time_pos: state.time_pos,
            duration: state.duration,
            paused: state.paused,
        };
        state.validate().ok()?;
        Some(HostMsg::LocalState { state })
    }
}

static WEB_MEDIA: Lazy<Mutex<WebMediaContext>> =
    Lazy::new(|| Mutex::new(WebMediaContext::default()));
static NATIVE_MEDIA: Lazy<Mutex<NativeMediaSnapshot>> =
    Lazy::new(|| Mutex::new(NativeMediaSnapshot::default()));
static READINESS_SNAPSHOT: Lazy<Mutex<Option<PartyReadiness>>> = Lazy::new(|| Mutex::new(None));

pub fn readiness_snapshot() -> Option<PartyReadiness> {
    READINESS_SNAPSHOT
        .lock()
        .ok()
        .and_then(|state| state.clone())
}

fn runtime_notification(command: RuntimeCommand) {
    if let Ok(runtime) = RUNTIME_TX.lock() {
        if let Some(tx) = runtime.as_ref() {
            let _ = tx.send(command);
        }
    }
}

pub fn update_web_media(
    media: Option<MediaIdentity>,
    selection_id: String,
    generation: Option<u64>,
    request_id: Option<String>,
    load_intent: bool,
) {
    let media = media.filter(|identity| identity.validate().is_ok());
    if selection_id.len() > 128 || request_id.as_ref().is_some_and(|id| id.len() > 128) {
        return;
    }
    let context = WebMediaContext {
        media,
        selection_id,
        generation,
        request_id,
    };
    let changed = if let Ok(mut current) = WEB_MEDIA.lock() {
        let changed = *current != context;
        *current = context.clone();
        changed
    } else {
        return;
    };
    if load_intent {
        if let Ok(mut native) = NATIVE_MEDIA.lock() {
            native.loaded = false;
            native.playback_ready = false;
            native.loading = context.media.is_some();
        }
    } else if let Ok(mut native) = NATIVE_MEDIA.lock() {
        // A current-core snapshot binds metadata for an already playing host.
        // Guest snapshots never carry a new selection request ticket.
        if native.loaded && context.request_id.is_none() {
            native.context = context.clone();
        }
    }
    if changed || load_intent {
        // Revoke immediately for real Player Load/Unload intents. Snapshot
        // reports are validated by bind_guest_context; a rejected stale ticket
        // must not revoke commands for the still-current loaded guest file.
        if load_intent
            && LOBBY_ROLE
                .lock()
                .map(|role| *role == LobbyRole::Guest)
                .unwrap_or(false)
        {
            invalidate_local_sync();
        }
        runtime_notification(RuntimeCommand::WebMediaChanged(context));
    }
}

pub fn notify_player_load_started(file_epoch: u64) {
    let context = WEB_MEDIA
        .lock()
        .map(|media| media.clone())
        .unwrap_or_default();
    if let Ok(mut native) = NATIVE_MEDIA.lock() {
        *native = NativeMediaSnapshot {
            context: context.clone(),
            file_epoch,
            loaded: false,
            loading: true,
            playback_ready: false,
        };
    }
    invalidate_local_sync();
    runtime_notification(RuntimeCommand::PlayerLoading {
        file_epoch,
        context,
    });
}

pub fn notify_player_loaded(file_epoch: u64) {
    if let Ok(mut native) = NATIVE_MEDIA.lock() {
        if native.file_epoch != file_epoch {
            return;
        }
        native.loaded = true;
    }
    runtime_notification(RuntimeCommand::PlayerLoaded { file_epoch });
}

pub fn notify_player_ready(file_epoch: u64) {
    if let Ok(mut native) = NATIVE_MEDIA.lock() {
        if !native.loaded
            || native.file_epoch != file_epoch
            || file_epoch != FILE_LOAD_EPOCH.load(Ordering::Acquire)
        {
            return;
        }
        if native.playback_ready {
            return;
        }
        native.playback_ready = true;
        native.loading = false;
    }
    runtime_notification(RuntimeCommand::PlayerReady { file_epoch });
}

pub fn notify_player_unloaded(file_epoch: u64) {
    if let Ok(mut native) = NATIVE_MEDIA.lock() {
        if native.file_epoch != file_epoch {
            return;
        }
        native.loaded = false;
        native.loading = false;
        native.playback_ready = false;
    }
    invalidate_local_sync();
    runtime_notification(RuntimeCommand::PlayerUnloaded { file_epoch });
}

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

pub fn start_host_lobby(
    party_id: String,
    max_size: i32,
    source_mode: SourceMode,
) -> Result<String, String> {
    if source_mode == SourceMode::Local
        && WEB_MEDIA
            .lock()
            .ok()
            .and_then(|context| context.media.clone())
            .is_none()
    {
        send_web_event("shell-watch-party-media-get", serde_json::json!({}));
        return Err("Local-source watch parties need a movie or episode opened from Stremio's stream selector. Wait for its metadata to load, then try again.".to_string());
    }
    let token = generate_token();
    let tx = ensure_runtime()?;
    let (reply_tx, reply_rx) = flume::bounded(1);

    tx.send(RuntimeCommand::StartHost {
        party_id,
        token,
        max_size,
        source_mode,
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
            tick_party_readiness(&client, &mut mode);
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
        tick_party_readiness(&client, &mut mode);
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
            source_mode,
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
                        source_mode,
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
        RuntimeCommand::WebMediaChanged(context) => {
            bind_guest_context(mode, &context, false);
        }
        RuntimeCommand::PlayerLoading {
            file_epoch,
            context,
        } => {
            if file_epoch == FILE_LOAD_EPOCH.load(Ordering::Acquire) {
                bind_guest_context(mode, &context, true);
                if let Some(Mode::Guest {
                    source_mode: SourceMode::Local,
                    local,
                    ..
                }) = mode
                {
                    local.loading(file_epoch);
                }
            }
        }
        RuntimeCommand::PlayerLoaded { file_epoch } => {
            if let Some(Mode::Guest {
                source_mode: SourceMode::Local,
                local,
                ..
            }) = mode
            {
                if file_epoch == FILE_LOAD_EPOCH.load(Ordering::Acquire) && local.loaded(file_epoch)
                {
                    if local.is_waiting() {
                        pause_local_playback(local);
                    } else {
                        apply_local_sync(local);
                    }
                }
            }
        }
        RuntimeCommand::PlayerReady { file_epoch } => {
            if file_epoch == FILE_LOAD_EPOCH.load(Ordering::Acquire) {
                if let Some(Mode::Guest {
                    source_mode: SourceMode::Local,
                    local,
                    ..
                }) = mode
                {
                    if !local.is_waiting() {
                        apply_local_sync(local);
                    }
                }
            }
        }
        RuntimeCommand::PlayerUnloaded { file_epoch } => {
            if file_epoch == FILE_LOAD_EPOCH.load(Ordering::Acquire) {
                if let Some(Mode::Guest {
                    source_mode: SourceMode::Local,
                    local,
                    ..
                }) = mode
                {
                    local.clear_selection();
                }
            }
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
            source_mode,
            reply,
        } => match result {
            Ok(lobby_id) => {
                let host_id = client.user().steam_id().raw();
                let matchmaking = client.matchmaking();
                matchmaking.set_lobby_data(lobby_id, "stremio_sync", "1");
                matchmaking.set_lobby_data(
                    lobby_id,
                    "protocol",
                    &STEAM_SYNC_PROTOCOL_VERSION.to_string(),
                );
                matchmaking.set_lobby_data(
                    lobby_id,
                    "source_mode",
                    if source_mode == SourceMode::Local {
                        "local"
                    } else {
                        "shared"
                    },
                );
                matchmaking.set_lobby_data(lobby_id, "app_id", &STEAM_SYNC_APP_ID.to_string());
                matchmaking.set_lobby_data(lobby_id, "host_steam_id", &host_id.to_string());
                matchmaking.set_lobby_data(lobby_id, "party_id", &party_id);
                matchmaking.set_lobby_joinable(lobby_id, true);

                *mode = Some(Mode::Host {
                    lobby_id,
                    token: token.clone(),
                    source_mode,
                    local_host: LocalHostPlayback::default(),
                    readiness: HostReadinessRoster::default(),
                    readiness_peers: HashSet::new(),
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
                    protocol_version: STEAM_SYNC_PROTOCOL_VERSION,
                    source_mode,
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
                    source_mode: secret.source_mode,
                    local: GuestLocalPlayback::default(),
                    readiness: GuestReadinessUpdates::default(),
                    last_context: None,
                    shared_wait_epoch: FILE_LOAD_EPOCH.load(Ordering::Acquire),
                });

                let hello = GuestMsg::Hello {
                    protocol_version: STEAM_SYNC_PROTOCOL_VERSION,
                    token: secret.token,
                    name: whoami::username(),
                    local_sources: true,
                    readiness: true,
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
            Some(Mode::Guest { host_steam_id, .. }) if *host_steam_id == peer_id => {
                match wire_msg {
                    SteamSyncWireMsg::Host(HostMsg::LobbyClosed { reason }) => {
                        leave_current_lobby(client, mode.take());
                        emit_ui_event(SyncUiEvent::HostLeft { reason });
                    }
                    SteamSyncWireMsg::Host(msg) => {
                        if let Some(guest) = mode.as_mut() {
                            if let Err(message) = handle_host_message(msg, guest) {
                                leave_current_lobby(client, mode.take());
                                emit_ui_event(SyncUiEvent::Error { message });
                            }
                        }
                    }
                    SteamSyncWireMsg::Error { message } => {
                        leave_current_lobby(client, mode.take());
                        emit_ui_event(SyncUiEvent::Error { message });
                    }
                    SteamSyncWireMsg::Guest(_) => {}
                }
            }
            _ => {}
        }
    }
}

fn handle_host_incoming(
    client: &steamworks::Client,
    mode: &mut Option<Mode>,
    peer_id: u64,
    wire_msg: SteamSyncWireMsg,
) {
    let (token, source_mode, max_size, authorized_peers, kicked_peers) = match mode {
        Some(Mode::Host {
            token,
            source_mode,
            max_size,
            authorized_peers,
            kicked_peers,
            ..
        }) => (
            token.clone(),
            *source_mode,
            *max_size,
            authorized_peers,
            kicked_peers,
        ),
        _ => return,
    };

    match wire_msg {
        SteamSyncWireMsg::Guest(GuestMsg::Hello {
            protocol_version,
            token: guest_token,
            name,
            local_sources,
            readiness: supports_readiness,
        }) => {
            if kicked_peers.contains(&peer_id) {
                send_error(client, peer_id, "you were removed from this watch party");
                return;
            }

            if !compatible_client(source_mode, protocol_version, local_sources) {
                send_error(client, peer_id, "Update Stremio Shell NG: this watch party requires compatible protocol v2 clients with local-source support.");
                return;
            }
            if guest_token != token {
                send_error(client, peer_id, "invalid watch-party token");
                return;
            }

            if !authorized_peers.contains_key(&peer_id)
                && authorized_peers.len() + 1 >= max_size as usize
            {
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
            send_current_host_state(client, mode, peer_id);
            let roster_changed = if let Some(Mode::Host {
                source_mode: SourceMode::Local,
                readiness,
                readiness_peers,
                ..
            }) = mode
            {
                if supports_readiness {
                    readiness_peers.insert(peer_id);
                } else {
                    readiness_peers.remove(&peer_id);
                }
                let _ = readiness.add_member(peer_id, supports_readiness);
                true
            } else {
                false
            };
            tick_party_readiness(client, mode);
            if roster_changed {
                broadcast_readiness(client, mode);
            } else {
                send_current_readiness(client, mode, peer_id);
            }
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
                send_current_host_state(client, mode, peer_id);
                send_current_readiness(client, mode, peer_id);
            }
        }
        SteamSyncWireMsg::Guest(GuestMsg::Readiness { state }) => {
            if let Some(Mode::Host {
                source_mode: SourceMode::Local,
                authorized_peers,
                readiness_peers,
                readiness,
                ..
            }) = mode
            {
                if authorized_peers.contains_key(&peer_id) && readiness_peers.contains(&peer_id) {
                    match readiness.report(peer_id, &state) {
                        Ok(true) => broadcast_readiness(client, mode),
                        Ok(false) => {}
                        Err(error) => {
                            eprintln!("Steam readiness: invalid report from {peer_id}: {error}")
                        }
                    }
                }
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
    remove_readiness_member(client, mode, peer_id);
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
    remove_readiness_member(client, mode, peer_id);
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
    if matches!(
        mode,
        Some(Mode::Host {
            source_mode: SourceMode::Local,
            ..
        })
    ) {
        let peers = match mode {
            Some(Mode::Host {
                authorized_peers,
                last_broadcast,
                ..
            }) => {
                if authorized_peers.is_empty() || last_broadcast.elapsed() < Duration::from_secs(1)
                {
                    return;
                }
                *last_broadcast = Instant::now();
                authorized_peers.keys().copied().collect::<Vec<_>>()
            }
            _ => return,
        };
        if let Some(message) = current_host_message(mode) {
            for peer in peers {
                send_host_msg(client, peer, message.clone(), true);
            }
        }
        return;
    }
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

fn compatible_client(mode: SourceMode, version: u16, local_sources: bool) -> bool {
    version == STEAM_SYNC_PROTOCOL_VERSION && (mode != SourceMode::Local || local_sources)
}

fn current_host_message(mode: &mut Option<Mode>) -> Option<HostMsg> {
    match mode {
        Some(Mode::Host {
            source_mode: SourceMode::Local,
            local_host,
            ..
        }) => {
            let native = NATIVE_MEDIA.lock().ok()?.clone();
            local_host.snapshot(
                &native,
                &gather_host_state(),
                FILE_LOAD_EPOCH.load(Ordering::Acquire),
            )
        }
        Some(Mode::Host {
            source_mode: SourceMode::Shared,
            ..
        }) => Some(gather_host_state().to_msg()),
        _ => None,
    }
}

fn send_current_host_state(client: &steamworks::Client, mode: &mut Option<Mode>, peer_id: u64) {
    if let Some(message) = current_host_message(mode) {
        send_host_msg(client, peer_id, message, true);
    }
}

fn publish_readiness(state: Option<PartyReadiness>) {
    let previous = if let Ok(mut current) = READINESS_SNAPSHOT.lock() {
        if *current == state {
            return;
        }
        std::mem::replace(&mut *current, state.clone())
    } else {
        return;
    };
    let announcement = readiness_announcement(
        previous.as_ref(),
        state.as_ref(),
        &LOBBY_MEMBERS
            .lock()
            .map(|members| members.clone())
            .unwrap_or_default(),
    );
    let media = state.as_ref().map(|state| state.media.clone());
    let generation = state.as_ref().map_or(0, |state| state.generation);
    let sequence = state.as_ref().map_or(0, |state| state.sequence);
    emit_ui_event(SyncUiEvent::ReadinessUpdated {
        announcement,
        media,
        generation,
        sequence,
    });
}

fn readiness_announcement(
    previous: Option<&PartyReadiness>,
    current: Option<&PartyReadiness>,
    members: &[LobbyMember],
) -> Option<String> {
    let current = current?;
    let previous = previous.filter(|previous| {
        previous.media == current.media && previous.generation == current.generation
    });
    let changes = current
        .members
        .iter()
        .filter_map(|row| {
            let old = previous.and_then(|previous| {
                previous
                    .members
                    .iter()
                    .find(|old| old.steam_id == row.steam_id)
            });
            if old.is_some_and(|old| old.status == row.status) {
                return None;
            }
            // The host may already have a decoded frame when a new episode's
            // first roster is published. Announce that Ready state too.
            if old.is_none() && row.status != Some(ReadinessStatus::Ready) {
                return None;
            }
            let name = &members
                .iter()
                .find(|member| member.steam_id == row.steam_id)?
                .name;
            Some(match row.status? {
                ReadinessStatus::Selecting => format!("{name} is choosing a source"),
                ReadinessStatus::Loading => format!("{name} is loading the video"),
                ReadinessStatus::Ready => format!("{name} is ready"),
            })
        })
        .collect::<Vec<_>>();
    (!changes.is_empty()).then(|| changes.join(". "))
}

fn send_current_readiness(client: &steamworks::Client, mode: &Option<Mode>, peer_id: u64) {
    if let Some(Mode::Host {
        source_mode: SourceMode::Local,
        readiness,
        readiness_peers,
        ..
    }) = mode
    {
        if readiness_peers.contains(&peer_id) {
            if let Some(state) = readiness.snapshot() {
                send_host_msg(client, peer_id, HostMsg::Readiness { state }, true);
            }
        }
    }
}

fn broadcast_readiness(client: &steamworks::Client, mode: &Option<Mode>) {
    if let Some(Mode::Host {
        source_mode: SourceMode::Local,
        readiness,
        readiness_peers,
        ..
    }) = mode
    {
        let state = readiness.snapshot();
        publish_readiness(state.clone());
        if let Some(state) = state {
            for peer_id in readiness_peers {
                send_host_msg(
                    client,
                    *peer_id,
                    HostMsg::Readiness {
                        state: state.clone(),
                    },
                    true,
                );
            }
        }
    }
}

fn remove_readiness_member(client: &steamworks::Client, mode: &mut Option<Mode>, peer_id: u64) {
    if let Some(Mode::Host {
        readiness,
        readiness_peers,
        ..
    }) = mode
    {
        readiness_peers.remove(&peer_id);
        if readiness.remove_member(peer_id).unwrap_or(false) {
            broadcast_readiness(client, mode);
        }
    }
}

fn host_readiness_status(
    native: &NativeMediaSnapshot,
    media: Option<&MediaIdentity>,
    current_epoch: u64,
) -> ReadinessStatus {
    let current_file = native.file_epoch == current_epoch
        && media.is_some_and(|media| native.context.media.as_ref() == Some(media));
    if current_file && native.loaded && native.playback_ready {
        ReadinessStatus::Ready
    } else if current_file && (native.loading || native.loaded) {
        ReadinessStatus::Loading
    } else {
        ReadinessStatus::Selecting
    }
}

fn tick_party_readiness(client: &steamworks::Client, mode: &mut Option<Mode>) {
    if matches!(
        mode,
        Some(Mode::Host {
            source_mode: SourceMode::Local,
            ..
        })
    ) {
        // Use the exact media generation assigned by playback synchronization.
        // If this discovers a new episode, send its state before its roster.
        let message = current_host_message(mode);
        let native = NATIVE_MEDIA
            .lock()
            .map(|native| native.clone())
            .unwrap_or_default();
        let host_id = client.user().steam_id().raw();
        let Some(Mode::Host {
            readiness,
            local_host,
            authorized_peers,
            ..
        }) = mode
        else {
            return;
        };
        let mut changed = readiness.add_member(host_id, true).unwrap_or(false);
        let new_media = if let Some(media) = local_host.media.as_ref() {
            readiness
                .set_media(media.clone(), local_host.generation)
                .unwrap_or(false)
        } else {
            false
        };
        changed |= new_media;
        let status = host_readiness_status(
            &native,
            local_host.media.as_ref(),
            FILE_LOAD_EPOCH.load(Ordering::Acquire),
        );
        changed |= readiness.set_status(host_id, status).unwrap_or(false);
        if new_media {
            if let Some(message) = message {
                for peer_id in authorized_peers.keys() {
                    send_host_msg(client, *peer_id, message.clone(), true);
                }
            }
        }
        if changed {
            broadcast_readiness(client, mode);
        }
    } else if let Some(Mode::Guest {
        source_mode: SourceMode::Local,
        local,
        readiness,
        host_steam_id,
        ..
    }) = mode
    {
        let native = NATIVE_MEDIA
            .lock()
            .map(|native| native.clone())
            .unwrap_or_default();
        if let Some(state) =
            readiness.report(local, &native, FILE_LOAD_EPOCH.load(Ordering::Acquire))
        {
            send_guest_msg(client, *host_steam_id, GuestMsg::Readiness { state }, true);
        }
    }
}

fn send_web_event(name: &str, value: serde_json::Value) {
    if let Ok(web) = crate::stremio_app::stremio_wevbiew::wevbiew::WEB_CMD_TX.lock() {
        if let Some(tx) = web.as_ref() {
            let _ = tx.send(crate::stremio_app::ipc::RPCResponse::response_message(
                Some(serde_json::json!([name, value])),
            ));
        }
    }
}

fn bind_guest_context(mode: &mut Option<Mode>, context: &WebMediaContext, loading: bool) {
    let Some(Mode::Guest {
        source_mode: SourceMode::Local,
        local,
        last_context,
        ..
    }) = mode
    else {
        return;
    };
    if !loading && last_context.as_ref() == Some(context) {
        return;
    }
    let Some(selection) = local.selection() else {
        if loading {
            local.clear_selection();
        }
        return;
    };
    if context.generation != Some(selection.generation) {
        // Unticketed current-core snapshots cannot authorize an old guest file.
        if loading || (context.generation.is_none() && context.request_id.is_none()) {
            local.clear_selection();
        }
        return;
    }
    let Some(request_id) = context.request_id.as_deref() else {
        if loading {
            local.clear_selection();
        }
        return;
    };
    let Some(media) = context.media.as_ref() else {
        local.clear_selection();
        return;
    };
    if local.select(media, request_id) {
        *last_context = Some(context.clone());
    } else if loading {
        // This is an accepted MPV load, so its context must independently match
        // the current request even when the previous file was already ready.
        local.clear_selection();
    }
}

fn apply_local_sync(local: &mut GuestLocalPlayback) {
    let native = NATIVE_MEDIA
        .lock()
        .map(|snapshot| snapshot.clone())
        .unwrap_or_default();
    if !native.loaded || native.file_epoch != FILE_LOAD_EPOCH.load(Ordering::Acquire) {
        return;
    }
    let time = CURRENT_TIME.lock().map(|time| *time).unwrap_or(0.0);
    if let Some(command) = local.command(Instant::now(), time, native.file_epoch) {
        send_player_cmd(&serde_json::json!(["shell-watch-party-sync", command]).to_string());
    }
}

fn pause_local_playback(local: &GuestLocalPlayback) {
    let native = NATIVE_MEDIA
        .lock()
        .map(|native| native.clone())
        .unwrap_or_default();
    if !native.loaded || !local.is_ready() || local.file_epoch() != Some(native.file_epoch) {
        return;
    }
    let pause = LocalSyncCommand {
        revision: current_local_sync_revision(),
        file_epoch: native.file_epoch,
        paused: true,
        seek_to: None,
    };
    send_player_cmd(&serde_json::json!(["shell-watch-party-sync", pause]).to_string());
}

fn apply_shared_sync(wait_epoch: u64, paused: bool, seek_to: Option<f64>) {
    let native = NATIVE_MEDIA
        .lock()
        .map(|snapshot| snapshot.clone())
        .unwrap_or_default();
    if !native.loaded
        || native.file_epoch <= wait_epoch
        || native.file_epoch != FILE_LOAD_EPOCH.load(Ordering::Acquire)
    {
        return;
    }
    if seek_to.is_some_and(|position| !position.is_finite() || position < 0.0) {
        return;
    }
    let command = LocalSyncCommand {
        revision: current_local_sync_revision(),
        file_epoch: native.file_epoch,
        paused,
        seek_to,
    };
    if !command.validate() {
        return;
    }
    send_player_cmd(&serde_json::json!(["shell-watch-party-sync", command]).to_string());
}

fn handle_host_message(msg: HostMsg, guest: &mut Mode) -> Result<(), String> {
    let Mode::Guest {
        current_web_url,
        source_mode,
        local,
        readiness,
        shared_wait_epoch,
        ..
    } = guest
    else {
        return Ok(());
    };
    if *source_mode == SourceMode::Local
        && !matches!(
            &msg,
            HostMsg::LocalState { .. }
                | HostMsg::LocalWaiting { .. }
                | HostMsg::Readiness { .. }
                | HostMsg::LobbyInfo { .. }
                | HostMsg::LobbyClosed { .. }
        )
    {
        return Err("The host sent an incompatible playback message. Update both clients; local-source parties cannot use shared media links.".to_string());
    }
    match msg {
        HostMsg::Readiness { state } => {
            if *source_mode != SourceMode::Local {
                return Err(
                    "The watch-party source mode does not match its readiness roster.".to_string(),
                );
            }
            let Some(selection) = local.selection() else {
                return Ok(());
            };
            if readiness
                .roster
                .ingest(state, &selection.media, selection.generation)?
            {
                readiness.supported = true;
                publish_readiness(
                    readiness
                        .roster
                        .current(&selection.media, selection.generation)
                        .cloned(),
                );
            }
        }
        HostMsg::LocalWaiting {
            generation,
            sequence,
        } => {
            if *source_mode != SourceMode::Local {
                return Err(
                    "The watch-party source mode does not match its invitation.".to_string()
                );
            }
            if local.waiting(generation, sequence)? && local.is_ready() {
                pause_local_playback(local);
            }
        }
        HostMsg::LocalState { state } => {
            if *source_mode != SourceMode::Local {
                return Err(
                    "The watch-party source mode does not match its invitation.".to_string()
                );
            }
            match local.ingest(state, Instant::now())? {
                GuestStateChange::Select(selection) => {
                    readiness.roster.clear();
                    publish_readiness(None);
                    send_web_event(
                        "shell-watch-party-select-source",
                        serde_json::json!({
                            "media": selection.media,
                            "generation": selection.generation,
                            "request_id": selection.request_id,
                        }),
                    );
                }
                GuestStateChange::Updated => apply_local_sync(local),
                GuestStateChange::Ignored => {}
            }
        }
        HostMsg::State {
            web_url,
            time_pos,
            paused,
            ..
        } => {
            if !web_url.is_empty() && web_url != *current_web_url {
                invalidate_local_sync();
                *shared_wait_epoch = FILE_LOAD_EPOCH.load(Ordering::Acquire);
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
                return Ok(());
            }

            let local_time = CURRENT_TIME.lock().map(|t| *t).unwrap_or(0.0);
            let local_duration = TOTAL_DURATION.lock().map(|t| *t).unwrap_or(0.0);

            let seek =
                (local_duration > 0.0 && (local_time - time_pos).abs() > 2.0).then_some(time_pos);
            apply_shared_sync(*shared_wait_epoch, paused, seek);
        }
        HostMsg::Load { .. } => {}
        HostMsg::Play { time_pos } => {
            apply_shared_sync(*shared_wait_epoch, false, Some(time_pos));
        }
        HostMsg::Pause { time_pos } => {
            apply_shared_sync(*shared_wait_epoch, true, Some(time_pos));
        }
        HostMsg::Seek { time_pos } => {
            let paused = IS_PAUSED.lock().map(|paused| *paused).unwrap_or(true);
            apply_shared_sync(*shared_wait_epoch, paused, Some(time_pos));
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
    Ok(())
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
            mut local,
            ..
        }) => {
            local.clear();
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
    invalidate_local_sync();
    publish_readiness(None);
    send_web_event("shell-watch-party-local-clear", serde_json::json!({}));
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

    fn native(video_id: &str, selection_id: &str, file_epoch: u64) -> NativeMediaSnapshot {
        NativeMediaSnapshot {
            context: WebMediaContext {
                media: Some(MediaIdentity {
                    content_type: "series".to_string(),
                    metadata_id: "tt1234567".to_string(),
                    video_id: video_id.to_string(),
                }),
                selection_id: selection_id.to_string(),
                ..WebMediaContext::default()
            },
            file_epoch,
            loaded: true,
            loading: false,
            playback_ready: true,
        }
    }

    fn playback() -> HostState {
        HostState {
            stream_url: "https://private-cdn.invalid/video?token=private-addon-secret".to_string(),
            web_url: "https://web.stremio.com/#/player/private-encoded-player-route".to_string(),
            time_pos: 42.0,
            duration: 3600.0,
            paused: false,
        }
    }

    fn guest() -> Mode {
        Mode::Guest {
            lobby_id: steamworks::LobbyId::from_raw(1),
            host_steam_id: 2,
            current_web_url: String::new(),
            source_mode: SourceMode::Local,
            local: GuestLocalPlayback::default(),
            readiness: GuestReadinessUpdates::default(),
            last_context: None,
            shared_wait_epoch: 0,
        }
    }

    fn local_state(message: HostMsg) -> LocalPlaybackState {
        match message {
            HostMsg::LocalState { state } => state,
            _ => panic!("expected identified local playback"),
        }
    }

    #[test]
    fn compatibility_gate_requires_version_and_local_source_capability() {
        let version = STEAM_SYNC_PROTOCOL_VERSION;
        assert!(compatible_client(SourceMode::Local, version, true));
        assert!(!compatible_client(SourceMode::Local, version, false));
        assert!(compatible_client(SourceMode::Shared, version, false));
        for mode in [SourceMode::Shared, SourceMode::Local] {
            assert!(!compatible_client(mode, version - 1, true));
            assert!(!compatible_client(mode, version + 1, true));
        }
    }

    #[test]
    fn local_host_snapshots_exclude_host_urls_and_addon_secrets() {
        let mut host = LocalHostPlayback::default();
        let mut native = native("tt1234567:1:1", "host-source", 10);
        let playback = playback();
        let identified = host.snapshot(&native, &playback, 10).unwrap();
        assert_eq!(
            serde_json::to_value(SteamSyncWireMsg::Host(identified)).unwrap(),
            serde_json::json!({
                "kind": "Host", "payload": {
                    "type": "local_state", "state": {
                        "media": {"content_type": "series", "metadata_id": "tt1234567", "video_id": "tt1234567:1:1"},
                        "generation": 1, "sequence": 1, "time_pos": 42.0,
                        "duration": 3600.0, "paused": false
                    }
                }
            })
        );
        native.loaded = false;
        let waiting = host.snapshot(&native, &playback, 10).unwrap();
        assert_eq!(
            serde_json::to_value(waiting).unwrap(),
            serde_json::json!({"type": "local_waiting", "generation": 1, "sequence": 2})
        );
        assert!(serde_json::to_string(&playback.to_msg())
            .unwrap()
            .contains("private-addon-secret"));
    }

    #[test]
    fn native_readiness_and_current_epoch_gate_host_identity() {
        let mut host = LocalHostPlayback::default();
        let mut native = native("tt1234567:1:1", "host-source", 10);
        assert!(matches!(
            host.snapshot(&native, &playback(), 11),
            Some(HostMsg::LocalWaiting { .. })
        ));
        native.context.media = None;
        assert!(matches!(
            host.snapshot(&native, &playback(), 10),
            Some(HostMsg::LocalWaiting { .. })
        ));
    }

    #[test]
    fn same_episode_host_reload_preserves_guest_source_and_catches_up() {
        let now = Instant::now();
        let mut host = LocalHostPlayback::default();
        let first = native("tt1234567:1:1", "host-source-one", 10);
        let initial = local_state(host.snapshot(&first, &playback(), 10).unwrap());
        let mut guest = GuestLocalPlayback::default();
        guest.ingest(initial.clone(), now).unwrap();
        let ticket = guest.selection().unwrap().request_id.clone();
        assert!(guest.select(&initial.media, &ticket));
        assert!(guest.loading(100));
        assert!(guest.loaded(100));
        let mut loading = first.clone();
        loading.loaded = false;
        if let HostMsg::LocalWaiting {
            generation,
            sequence,
        } = host.snapshot(&loading, &playback(), 10).unwrap()
        {
            assert!(guest.waiting(generation, sequence).unwrap());
        } else {
            panic!("expected loading state");
        }
        let reloaded = native("tt1234567:1:1", "host-source-two", 11);
        let mut position = playback();
        position.time_pos = 120.0;
        let recovered = local_state(host.snapshot(&reloaded, &position, 11).unwrap());
        assert_eq!(recovered.generation, initial.generation);
        assert_eq!(
            guest.ingest(recovered, now).unwrap(),
            GuestStateChange::Updated
        );
        assert!(guest.is_ready());
        assert_eq!(guest.selection().unwrap().request_id, ticket);
        let command = guest.command(now, 42.0, 100).unwrap();
        assert_eq!(command.seek_to, Some(120.0));
        assert_eq!(command.file_epoch, 100);

        let other_episode = native("tt1234567:1:2", "host-source-three", 12);
        let other = local_state(host.snapshot(&other_episode, &position, 12).unwrap());
        assert!(other.generation > initial.generation);
        assert!(matches!(
            guest.ingest(other, now).unwrap(),
            GuestStateChange::Select(_)
        ));
        assert!(!guest.is_ready());
        let return_episode = native("tt1234567:1:1", "host-source-four", 13);
        let returned = local_state(host.snapshot(&return_episode, &position, 13).unwrap());
        assert_eq!(returned.generation, 3);
        assert_eq!(
            guest.ingest(initial, now).unwrap(),
            GuestStateChange::Ignored
        );
    }

    #[test]
    fn local_guest_rejects_every_legacy_playback_message() {
        for message in [
            playback().to_msg(),
            HostMsg::Load {
                stream_url: "https://private.invalid".to_string(),
                web_url: "#/player/private".to_string(),
            },
            HostMsg::Play { time_pos: 42.0 },
            HostMsg::Pause { time_pos: 42.0 },
            HostMsg::Seek { time_pos: 42.0 },
        ] {
            let mut guest = guest();
            assert!(handle_host_message(message, &mut guest).is_err());
            let Mode::Guest {
                current_web_url,
                local,
                ..
            } = guest
            else {
                unreachable!()
            };
            assert!(current_web_url.is_empty());
            assert!(local.selection().is_none());
        }
    }

    #[test]
    fn full_ticket_context_is_deduplicated_and_stale_selection_cannot_bind_new_episode() {
        let mut host = LocalHostPlayback::default();
        let initial = local_state(
            host.snapshot(&native("tt1234567:1:1", "host", 10), &playback(), 10)
                .unwrap(),
        );
        let mut mode = Some(guest());
        let Some(Mode::Guest { local, .. }) = &mut mode else {
            unreachable!()
        };
        local.ingest(initial, Instant::now()).unwrap();
        let selection = local.selection().unwrap();
        let old_context = WebMediaContext {
            media: Some(selection.media.clone()),
            selection_id: "guest-local-selection".to_string(),
            generation: Some(selection.generation),
            request_id: Some(selection.request_id.clone()),
        };
        bind_guest_context(&mut mode, &old_context, false);
        let Some(Mode::Guest { local, .. }) = &mut mode else {
            unreachable!()
        };
        assert!(local.loading(100));
        assert!(local.loaded(100));
        bind_guest_context(&mut mode, &old_context, false);
        let Some(Mode::Guest { local, .. }) = &mut mode else {
            unreachable!()
        };
        assert!(local.is_ready());

        let next = local_state(
            host.snapshot(&native("tt1234567:1:2", "host-next", 11), &playback(), 11)
                .unwrap(),
        );
        local.ingest(next, Instant::now()).unwrap();
        let selection = local.selection().unwrap();
        let new_context = WebMediaContext {
            media: Some(selection.media.clone()),
            generation: Some(selection.generation),
            request_id: Some(selection.request_id.clone()),
            ..old_context.clone()
        };
        bind_guest_context(&mut mode, &old_context, true);
        let Some(Mode::Guest { local, .. }) = &mut mode else {
            unreachable!()
        };
        assert!(!local.loading(101));
        bind_guest_context(&mut mode, &new_context, false);
        let Some(Mode::Guest { local, .. }) = &mut mode else {
            unreachable!()
        };
        assert!(local.loading(102));
        assert!(!local.loaded(101));
        assert!(local.loaded(102));

        // A delayed snapshot is not a new native load and cannot revoke the
        // current file's synchronization revision or its validated binding.
        let prior_web = WEB_MEDIA.lock().unwrap().clone();
        let prior_role = *LOBBY_ROLE.lock().unwrap();
        *LOBBY_ROLE.lock().unwrap() = LobbyRole::Guest;
        let revision = current_local_sync_revision();
        update_web_media(
            old_context.media.clone(),
            old_context.selection_id.clone(),
            old_context.generation,
            old_context.request_id.clone(),
            false,
        );
        let revision_after_snapshot = current_local_sync_revision();
        *WEB_MEDIA.lock().unwrap() = prior_web;
        *LOBBY_ROLE.lock().unwrap() = prior_role;
        bind_guest_context(&mut mode, &old_context, false);
        let Some(Mode::Guest { local, .. }) = &mut mode else {
            unreachable!()
        };
        assert_eq!(revision_after_snapshot, revision);
        assert!(local.is_ready());
        assert_eq!(
            local.command(Instant::now(), 42.0, 102).unwrap().revision,
            revision
        );

        // An accepted new native load with that same stale context must clear
        // the old selection rather than inherit selected=true from this file.
        bind_guest_context(&mut mode, &old_context, true);
        let Some(Mode::Guest { local, .. }) = &mut mode else {
            unreachable!()
        };
        assert!(!local.loading(103));
        assert!(!local.loaded(103));
        bind_guest_context(&mut mode, &new_context, true);
        let Some(Mode::Guest { local, .. }) = &mut mode else {
            unreachable!()
        };
        assert!(local.loading(104));
        assert!(local.loaded(104));
        let wrong_request = WebMediaContext {
            request_id: Some("obsolete-request-for-same-generation".to_string()),
            ..new_context
        };
        bind_guest_context(&mut mode, &wrong_request, true);
        let Some(Mode::Guest { local, .. }) = &mut mode else {
            unreachable!()
        };
        assert!(!local.loading(105));
        assert!(!local.loaded(105));
    }

    #[test]
    fn guest_session_accepts_only_the_invited_host() {
        let policy = SessionPolicy {
            role: LobbyRole::Guest,
            host_steam_id: Some(42),
        };
        assert!(policy.should_accept(42));
        assert!(!policy.should_accept(43));
    }

    #[test]
    fn readiness_reports_wait_for_host_support_and_current_file_first_frame() {
        let now = Instant::now();
        let mut host = LocalHostPlayback::default();
        let initial = local_state(
            host.snapshot(&native("tt1234567:1:1", "host", 10), &playback(), 10)
                .unwrap(),
        );
        let mut local = GuestLocalPlayback::default();
        local.ingest(initial.clone(), now).unwrap();
        let ticket = local.selection().unwrap().request_id.clone();
        let mut updates = GuestReadinessUpdates::default();
        let mut file = native("tt1234567:1:1", "guest", 100);
        assert!(updates.report(&local, &file, 100).is_none());
        updates.supported = true;
        let selecting = updates.report(&local, &file, 100).unwrap();
        assert_eq!(selecting.status, ReadinessStatus::Selecting);
        assert!(local.select(&initial.media, &ticket));
        assert!(local.loading(100));
        file.loaded = false;
        file.playback_ready = false;
        file.loading = true;
        let loading = updates.report(&local, &file, 100).unwrap();
        assert_eq!(loading.status, ReadinessStatus::Loading);
        assert!(loading.sequence > selecting.sequence);
        assert!(local.loaded(100));
        file.loaded = true;
        assert!(updates.report(&local, &file, 100).is_none());
        file.playback_ready = true;
        file.loading = false;
        let ready = updates.report(&local, &file, 100).unwrap();
        assert_eq!(ready.status, ReadinessStatus::Ready);
        assert!(ready.sequence > loading.sequence);
        assert!(updates.report(&local, &file, 100).is_none());
        assert!(local.waiting(1, 2).unwrap());
        assert!(updates.report(&local, &file, 100).is_none());

        assert!(local.select(&initial.media, &ticket));
        assert!(local.loading(101));
        file.file_epoch = 101;
        file.loaded = false;
        file.playback_ready = false;
        file.loading = true;
        let reload = updates.report(&local, &file, 101).unwrap();
        assert_eq!(reload.status, ReadinessStatus::Loading);
        assert!(reload.sequence > ready.sequence);
        let stale_file = native("tt1234567:1:1", "obsolete", 100);
        assert_eq!(
            updates.report(&local, &stale_file, 101).unwrap().status,
            ReadinessStatus::Selecting
        );
        let mut next = initial;
        next.media.video_id = "tt1234567:1:2".to_string();
        next.generation = 2;
        next.sequence = 3;
        local.ingest(next.clone(), now).unwrap();
        let episode = updates.report(&local, &stale_file, 101).unwrap();
        assert_eq!(episode.status, ReadinessStatus::Selecting);
        assert_eq!(episode.media, next.media);
        assert_eq!(episode.generation, 2);
    }

    #[test]
    fn host_readiness_requires_the_announced_episode_and_current_native_frame() {
        let mut file = native("tt1234567:1:1", "host", 10);
        let media = file.context.media.clone().unwrap();
        assert_eq!(
            host_readiness_status(&file, Some(&media), 10),
            ReadinessStatus::Ready
        );
        assert_eq!(
            host_readiness_status(&file, Some(&media), 11),
            ReadinessStatus::Selecting
        );
        file.playback_ready = false;
        assert_eq!(
            host_readiness_status(&file, Some(&media), 10),
            ReadinessStatus::Loading
        );
        file.playback_ready = true;
        file.context.media = None;
        assert_eq!(
            host_readiness_status(&file, Some(&media), 10),
            ReadinessStatus::Selecting
        );
        file.context.media = native("tt1234567:1:2", "another-file", 10).context.media;
        assert_eq!(
            host_readiness_status(&file, Some(&media), 10),
            ReadinessStatus::Selecting
        );
        assert_eq!(
            host_readiness_status(&file, None, 10),
            ReadinessStatus::Selecting
        );
    }

    #[test]
    fn guest_readiness_handshake_and_visible_roster_follow_current_episode_only() {
        let previous_snapshot = readiness_snapshot();
        let mut guest = guest();
        let mut host = LocalHostPlayback::default();
        let initial = local_state(
            host.snapshot(&native("tt1234567:1:1", "host", 10), &playback(), 10)
                .unwrap(),
        );
        handle_host_message(
            HostMsg::LocalState {
                state: initial.clone(),
            },
            &mut guest,
        )
        .unwrap();
        let mut roster = HostReadinessRoster::default();
        roster.add_member(2, true).unwrap();
        roster.add_member(3, true).unwrap();
        roster
            .set_media(initial.media.clone(), initial.generation)
            .unwrap();
        roster.set_status(2, ReadinessStatus::Ready).unwrap();
        let current = roster.snapshot().unwrap();
        handle_host_message(
            HostMsg::Readiness {
                state: current.clone(),
            },
            &mut guest,
        )
        .unwrap();
        let snapshot = readiness_snapshot();
        let mut stale = current.clone();
        stale.generation += 1;
        stale.sequence += 100;
        handle_host_message(HostMsg::Readiness { state: stale }, &mut guest).unwrap();
        let after_stale = readiness_snapshot();
        let Mode::Guest { readiness, .. } = &guest else {
            unreachable!()
        };
        let supported = readiness.supported;
        let mut next = initial;
        next.media.video_id = "tt1234567:1:2".to_string();
        next.generation += 1;
        next.sequence += 1;
        handle_host_message(HostMsg::LocalState { state: next }, &mut guest).unwrap();
        let cleared = readiness_snapshot();
        handle_host_message(
            HostMsg::Readiness {
                state: current.clone(),
            },
            &mut guest,
        )
        .unwrap();
        let after_old_episode = readiness_snapshot();
        *READINESS_SNAPSHOT.lock().unwrap() = previous_snapshot;
        assert!(supported);
        assert_eq!(snapshot, Some(current));
        assert_eq!(after_stale, snapshot);
        assert!(cleared.is_none());
        assert!(after_old_episode.is_none());
    }

    #[test]
    fn readiness_announcements_are_named_transitions_without_heartbeat_or_episode_noise() {
        let mut roster = HostReadinessRoster::default();
        roster.add_member(1, true).unwrap();
        roster.add_member(2, true).unwrap();
        roster
            .set_media(
                native("tt1234567:1:1", "host", 10).context.media.unwrap(),
                1,
            )
            .unwrap();
        let members = vec![
            LobbyMember {
                steam_id: 1,
                name: "Host".to_string(),
                is_host: true,
            },
            LobbyMember {
                steam_id: 2,
                name: "Alice".to_string(),
                is_host: false,
            },
        ];
        let selecting = roster.snapshot().unwrap();
        assert!(readiness_announcement(None, Some(&selecting), &members).is_none());
        roster.set_status(2, ReadinessStatus::Loading).unwrap();
        let loading = roster.snapshot().unwrap();
        assert_eq!(
            readiness_announcement(Some(&selecting), Some(&loading), &members),
            Some("Alice is loading the video".to_string())
        );
        roster.set_status(2, ReadinessStatus::Ready).unwrap();
        let ready = roster.snapshot().unwrap();
        assert_eq!(
            readiness_announcement(Some(&loading), Some(&ready), &members),
            Some("Alice is ready".to_string())
        );
        assert!(readiness_announcement(Some(&ready), Some(&ready), &members).is_none());
        roster
            .set_media(native("tt1234567:1:2", "new", 11).context.media.unwrap(), 2)
            .unwrap();
        assert!(
            readiness_announcement(Some(&ready), roster.snapshot().as_ref(), &members).is_none()
        );
        roster.set_status(1, ReadinessStatus::Ready).unwrap();
        let next_ready = roster.snapshot().unwrap();
        assert_eq!(
            readiness_announcement(Some(&ready), Some(&next_ready), &members),
            Some("Host is ready".to_string())
        );
        assert_eq!(
            readiness_announcement(None, Some(&next_ready), &members),
            Some("Host is ready".to_string())
        );
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
