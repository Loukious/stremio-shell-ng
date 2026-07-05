use discord_rich_presence::{
    activity::{
        Activity, ActivityType, Assets, Button, Party, Secrets, Timestamps,
    },
    DiscordIpc, DiscordIpcClient,
};
use flume::{Receiver, Sender};
use ini::Ini;
use native_windows_derive::NwgUi;
use native_windows_gui as nwg;
use once_cell::sync::Lazy;
use reqwest::blocking::Client;
use serde_json::{self, Value};
use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    env,
    ffi::c_void,
    io::Read,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{self, Command},
    str,
    sync::{Arc, Mutex},
    thread,
    time::{self, Duration, SystemTime, UNIX_EPOCH},
};
use url::Url;
use urlencoding::decode;
use winapi::shared::windef::{HBRUSH, HDC, HWND, RECT};
use winapi::um::wingdi::{GetStockObject, BLACK_BRUSH};
use winapi::um::{
    winbase::CREATE_BREAKAWAY_FROM_JOB,
    winuser::{FillRect, GetClientRect, WM_ERASEBKGND, WS_EX_TOPMOST},
};
struct SafeHwnd(*mut c_void);
unsafe impl Send for SafeHwnd {}

pub static VIDEO_TITLE: Lazy<Mutex<String>> = Lazy::new(|| Mutex::new("".to_string()));

pub static COVER_URL: Lazy<Mutex<String>> = Lazy::new(|| Mutex::new("".to_string()));

pub static ALBUM: Lazy<Mutex<String>> = Lazy::new(|| Mutex::new("".to_string()));

pub const ICON_URL: &str =
    "https://raw.githubusercontent.com/Stremio/stremio-web/refs/heads/development/assets/images/icon.png";

// ── Lobby / Watch-Party state ──────────────────────────────────────────
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum LobbyRole {
    None,
    Host,
    Guest,
}

/// Unique party ID for the current lobby (empty = no active lobby)
pub static LOBBY_PARTY_ID: Lazy<Mutex<String>> = Lazy::new(|| Mutex::new(String::new()));
/// Opaque join secret shared via Discord RPC
pub static LOBBY_JOIN_SECRET: Lazy<Mutex<String>> = Lazy::new(|| Mutex::new(String::new()));
/// Current number of members in the lobby (including the host)
pub static LOBBY_MEMBER_COUNT: Lazy<Mutex<i32>> = Lazy::new(|| Mutex::new(0));
/// Maximum lobby size
pub static LOBBY_MAX_SIZE: Lazy<Mutex<i32>> = Lazy::new(|| Mutex::new(8));
/// Host-authored member list for the active watch party.
pub static LOBBY_MEMBERS: Lazy<Mutex<Vec<crate::stremio_app::sync_protocol::LobbyMember>>> =
    Lazy::new(|| Mutex::new(Vec::new()));
/// Whether this app owns the lobby or joined someone else's lobby.
pub static LOBBY_ROLE: Lazy<Mutex<LobbyRole>> = Lazy::new(|| Mutex::new(LobbyRole::None));
use crate::stremio_app::{
    constants::{
        web_endpoint_with_streaming_server, APP_NAME, UPDATE_ENDPOINT, UPDATE_INTERVAL,
        WEB_ENDPOINT, WINDOW_MIN_HEIGHT, WINDOW_MIN_WIDTH,
    },
    ipc::{RPCRequest, RPCResponse},
    mpv_hwnd::find_mpv_child_hwnd,
    pip_window::{PipBuildContext, PipWindow},
    splash::SplashImage,
    stremio_player::{
        player::{CURRENT_TIME, IS_PAUSED, TOTAL_DURATION},
        Player,
    },
    stremio_wevbiew::{wevbiew::CURRENT_URL, WebView},
    systray::SystemTray,
    updater,
    window_helper::WindowStyle,
    window_settings::{PipPlacement, WindowSettings},
    PipeServer,
};

use super::discord::DiscordRpc;
use super::stremio_server::StremioServer;

fn weserv_contain(url: &str) -> String {
    format!(
        "https://images.weserv.nl/?url={}&w=1024&h=1024&fit=contain",
        urlencoding::encode(url)
    )
}

#[derive(Debug, Default)]
pub struct VideoInfo {
    pub poster: String,
    pub name: String,
    pub year: String,
    pub thumbnail: String,
    pub epname: String,
}

struct Config {
    show_buttons: bool,
    link_target: String,
    disable_in_menu: bool,
    disable_when_paused: bool,
    refresh_interval: u64,
    show_small_image: bool,
    swap_name_and_title: bool,
    lobby_max_size: i32,
    auto_skip_enabled: bool,
    auto_skip_intro: bool,
    auto_skip_recap: bool,
    auto_skip_outro: bool,
    chapter_intro_words: Vec<String>,
    chapter_recap_words: Vec<String>,
    chapter_outro_words: Vec<String>,
    introdb_api_key: Option<String>,
    theintrodb_api_key: Option<String>,
}

#[derive(Debug, Clone)]
struct LobbyPresence {
    party_id: String,
    join_secret: String,
    member_count: i32,
    max_size: i32,
}

impl LobbyPresence {
    fn others_text(&self) -> Option<String> {
        let others = self.member_count.saturating_sub(1);
        match others {
            0 => None,
            1 => Some("with 1 other".to_string()),
            n => Some(format!("with {n} others")),
        }
    }
}

fn lobby_presence(config: &Config) -> Option<LobbyPresence> {
    let party_id = LOBBY_PARTY_ID.lock().map(|p| p.clone()).unwrap_or_default();
    if party_id.is_empty() {
        return None;
    }

    let join_secret = LOBBY_JOIN_SECRET
        .lock()
        .map(|s| s.clone())
        .unwrap_or_default();
    let member_count = LOBBY_MEMBER_COUNT.lock().map(|c| *c).unwrap_or(1).max(1);
    let max_size = LOBBY_MAX_SIZE
        .lock()
        .map(|m| *m)
        .unwrap_or(config.lobby_max_size)
        .max(member_count);

    Some(LobbyPresence {
        party_id,
        join_secret,
        member_count,
        max_size,
    })
}

fn send_webview_script(script: &str) {
    use crate::stremio_app::stremio_wevbiew::wevbiew::{WEBVIEW_EXEC_SCRIPT_PREFIX, WEB_CMD_TX};

    if let Ok(tx) = WEB_CMD_TX.lock() {
        if let Some(tx) = tx.as_ref() {
            let _ = tx.send(format!("{WEBVIEW_EXEC_SCRIPT_PREFIX}{script}"));
        }
    }
}

fn handle_media_status(params: Option<&Value>) {
    let Some(params) = params else {
        return;
    };

    if let Some(paused) = params.get("paused").and_then(Value::as_bool) {
        if let Ok(mut is_paused) = IS_PAUSED.lock() {
            *is_paused = paused;
        }
    }

    for key in ["time", "timePos", "position", "currentTime"] {
        if let Some(time) = params.get(key).and_then(Value::as_f64) {
            if let Ok(mut current_time) = CURRENT_TIME.lock() {
                *current_time = time;
            }
            break;
        }
    }

    if let Some(duration) = params.get("duration").and_then(Value::as_f64) {
        if let Ok(mut total_duration) = TOTAL_DURATION.lock() {
            *total_duration = duration;
        }
    }
}

fn handle_media_metadata(params: Option<&Value>) {
    let Some(params) = params else {
        return;
    };

    if let Some(title) = params.get("title").and_then(Value::as_str) {
        if let Ok(mut video_title) = VIDEO_TITLE.lock() {
            *video_title = title.to_string();
        }
    }

    if let Some(album) = params
        .get("artist")
        .or_else(|| params.get("album"))
        .and_then(Value::as_str)
    {
        if let Ok(mut album_guard) = ALBUM.lock() {
            *album_guard = album.to_string();
        }
    }

    if let Some(cover) = params
        .get("artUrl")
        .or_else(|| params.get("poster"))
        .or_else(|| params.get("thumbnail"))
        .and_then(Value::as_str)
    {
        if let Ok(mut cover_url) = COVER_URL.lock() {
            *cover_url = cover.to_string();
        }
    }
}

#[derive(Default, NwgUi)]
pub struct MainWindow {
    pub command: String,
    pub commands_path: Option<String>,
    pub webui_url: String,
    pub no_splash: bool,
    pub dev_tools: bool,
    pub start_hidden: bool,
    pub autoupdater_endpoint: Option<Url>,
    pub force_update: bool,
    pub release_candidate: bool,
    pub autoupdater_setup_file: Arc<Mutex<Option<PathBuf>>>,
    pub requested_fullscreen: Arc<Mutex<Option<bool>>>,
    pub requested_pip: Arc<Mutex<Option<bool>>>,
    pub pip_window: RefCell<PipWindow>,
    pub pip_active: Cell<bool>,
    pub mpv_child_hwnd: Cell<Option<HWND>>,
    pub saved_window_style: RefCell<WindowStyle>,
    #[nwg_resource]
    pub embed: nwg::EmbedResource,
    #[nwg_resource(source_embed: Some(&data.embed), source_embed_str: Some("MAINICON"))]
    pub window_icon: nwg::Icon,
    #[nwg_control(icon: Some(&data.window_icon), title: APP_NAME, flags: "MAIN_WINDOW")]
    #[nwg_events(
        OnWindowClose: [Self::on_quit(SELF, EVT_DATA)],
        OnInit: [Self::on_init],
        OnPaint: [Self::on_paint],
        OnMinMaxInfo: [Self::on_min_max(SELF, EVT_DATA)],
        OnWindowMinimize: [Self::transmit_window_state_change],
        OnWindowMaximize: [Self::on_window_state_changed],
        OnWindowFocus: [Self::transmit_window_state_change],
        OnResizeEnd: [Self::save_window_settings],
    )]
    pub window: nwg::Window,
    #[nwg_partial(parent: window)]
    #[nwg_events(
        (tray, MousePressLeftUp): [Self::on_show],
        (tray_exit, OnMenuItemSelected): [Self::on_exit],
        (tray_start_watch_party, OnMenuItemSelected): [Self::on_start_watch_party],
        (tray_end_watch_party, OnMenuItemSelected): [Self::on_end_watch_party],
        (tray_leave_watch_party, OnMenuItemSelected): [Self::on_leave_watch_party],
        (tray_show_hide, OnMenuItemSelected): [Self::on_show_hide],
        (tray_topmost, OnMenuItemSelected): [Self::on_toggle_topmost],
        (tray_pip, OnMenuItemSelected): [Self::on_tray_toggle_pip],
    )]
    pub tray: SystemTray,
    #[nwg_partial(parent: window)]
    pub splash_screen: SplashImage,
    #[nwg_partial(parent: window)]
    pub server: StremioServer,
    #[nwg_partial(parent: window)]
    pub player: Player,
    #[nwg_partial(parent: window)]
    pub webview: WebView,
    #[nwg_control]
    #[nwg_events(OnNotice: [Self::on_toggle_fullscreen_notice] )]
    pub toggle_fullscreen_notice: nwg::Notice,
    #[nwg_control]
    #[nwg_events(OnNotice: [Self::on_toggle_pip_notice] )]
    pub toggle_pip_notice: nwg::Notice,
    #[nwg_control]
    #[nwg_events(OnNotice: [Self::on_exit] )]
    pub quit_notice: nwg::Notice,
    #[nwg_control]
    #[nwg_events(OnNotice: [Self::on_hide_splash_notice] )]
    pub hide_splash_notice: nwg::Notice,
    #[nwg_control]
    #[nwg_events(OnNotice: [Self::on_focus_notice] )]
    pub focus_notice: nwg::Notice,
    #[nwg_control]
    #[nwg_events(OnNotice: [Self::on_sync_notice] )]
    pub sync_notice: nwg::Notice,
    pub sync_events: Arc<Mutex<VecDeque<crate::stremio_app::steam_sync::SyncUiEvent>>>,
}

fn load_or_create_config() -> Config {
    // Get the path to the configuration file
    let exe_path = env::current_exe().expect("Failed to get executable path");
    let exe_dir = exe_path
        .parent()
        .expect("Failed to get executable directory");
    let config_path = exe_dir.join("RPCconfig.ini");

    // Check if the config file exists, create it if not
    if !config_path.exists() {
        let mut default_config = Ini::new();
        default_config
            .with_section(Some("Buttons"))
            .set("show_buttons", "true")
            .set("link_target", "app");
        default_config
            .with_section(Some("Activity"))
            .set("disable_in_menu", "false")
            .set("disable_when_paused", "false")
            .set("refresh_interval", "5")
            .set("show_small_image", "true")
            .set("swap_name_and_title", "false");
        default_config
            .with_section(Some("Lobby"))
            .set("lobby_max_size", "8");

        default_config
            .with_section(Some("AutoSkip"))
            .set("enabled", "true")
            .set("skip_intro", "true")
            .set("skip_recap", "true")
            .set("skip_outro", "true");

        default_config
            .with_section(Some("AutoSkipChapters"))
            // Comma-separated keywords matched against the current chapter title (case-insensitive).
            // Each list is gated by the corresponding [AutoSkip] toggle (skip_intro/skip_recap/skip_outro).
            // Leave a value blank to disable matching for that kind.
            .set("intro", "opening,intro,logo")
            .set("recap", "recap")
            .set("outro", "credits,outro,ending");

        default_config
            .with_section(Some("IntroDB"))
            .set("api_key", "");

        default_config
            .with_section(Some("TheIntroDB"))
            .set("api_key", "");

        default_config
            .write_to_file(&config_path)
            .expect("Failed to create configuration file");
        println!(
            "Default configuration file created at '{}'",
            config_path.display()
        );
    }

    // Load the configuration file
    let config = Ini::load_from_file(&config_path).unwrap_or_else(|_| {
        panic!(
            "Failed to load configuration file: {}",
            config_path.display()
        )
    });

    fn parse_word_list(raw: &str) -> Vec<String> {
        raw.split(',')
            .map(|w| w.trim())
            .filter(|w| !w.is_empty())
            .map(|w| w.to_lowercase())
            .collect()
    }

    // Parse values from the configuration file
    let show_buttons = config
        .section(Some("Buttons"))
        .and_then(|sec| sec.get("show_buttons"))
        .map(|value| value == "true")
        .unwrap_or(true);

    let link_target = config
        .section(Some("Buttons"))
        .and_then(|sec| sec.get("link_target").map(|value| value.to_string()))
        .unwrap_or_else(|| "app".to_string());

    let disable_in_menu = config
        .section(Some("Activity"))
        .and_then(|sec| sec.get("disable_in_menu"))
        .map(|value| value == "true")
        .unwrap_or(false);

    let disable_when_paused = config
        .section(Some("Activity"))
        .and_then(|sec| sec.get("disable_when_paused"))
        .map(|value| value == "true")
        .unwrap_or(false);

    let refresh_interval = config
        .section(Some("Activity"))
        .and_then(|sec| sec.get("refresh_interval"))
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(5);

    let show_small_image = config
        .section(Some("Activity"))
        .and_then(|sec| sec.get("show_small_image"))
        .map(|value| value == "true")
        .unwrap_or(true);

    let swap_name_and_title = config
        .section(Some("Activity"))
        .and_then(|sec| sec.get("swap_name_and_title"))
        .map(|value| value == "true")
        .unwrap_or(false);

    let lobby_max_size = config
        .section(Some("Lobby"))
        .and_then(|sec| sec.get("lobby_max_size"))
        .and_then(|value| value.parse::<i32>().ok())
        .unwrap_or(8)
        .clamp(2, 16);

    // Backward-compat: older configs used [IntroDB] enabled=true as the master toggle.
    let legacy_introdb_enabled = config
        .section(Some("IntroDB"))
        .and_then(|sec| sec.get("enabled"))
        .map(|value| value == "true");

    let auto_skip_enabled = config
        .section(Some("AutoSkip"))
        .and_then(|sec| sec.get("enabled"))
        .map(|value| value == "true")
        .or(legacy_introdb_enabled)
        .unwrap_or(true);

    let auto_skip_intro = config
        .section(Some("AutoSkip"))
        .and_then(|sec| sec.get("skip_intro"))
        .map(|value| value == "true")
        .unwrap_or(true);

    let auto_skip_recap = config
        .section(Some("AutoSkip"))
        .and_then(|sec| sec.get("skip_recap"))
        .map(|value| value == "true")
        .unwrap_or(true);

    let auto_skip_outro = config
        .section(Some("AutoSkip"))
        .and_then(|sec| sec.get("skip_outro"))
        .map(|value| value == "true")
        .unwrap_or(true);

    // Chapter-title keyword lists (comma-separated)
    // Matches are case-insensitive substring checks against `chapter-metadata/by-key/title`.
    let (chapter_intro_words, chapter_recap_words, chapter_outro_words) =
        if let Some(sec) = config.section(Some("AutoSkipChapters")) {
            (
                parse_word_list(sec.get("intro").unwrap_or("opening,intro,logo")),
                parse_word_list(sec.get("recap").unwrap_or("recap")),
                parse_word_list(sec.get("outro").unwrap_or("credits,outro,ending")),
            )
        } else {
            (
                parse_word_list("opening,intro,logo"),
                parse_word_list("recap"),
                parse_word_list("credits,outro,ending"),
            )
        };

    let introdb_api_key = config
        .section(Some("IntroDB"))
        .and_then(|sec| sec.get("api_key"))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    let theintrodb_api_key = config
        .section(Some("TheIntroDB"))
        .and_then(|sec| sec.get("api_key"))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    Config {
        show_buttons,
        link_target,
        disable_in_menu,
        disable_when_paused,
        refresh_interval,
        show_small_image,
        swap_name_and_title,
        lobby_max_size,
        auto_skip_enabled,
        auto_skip_intro,
        auto_skip_recap,
        auto_skip_outro,
        chapter_intro_words,
        chapter_recap_words,
        chapter_outro_words,
        introdb_api_key,
        theintrodb_api_key,
    }
}

pub fn getvidinfo(type_: &str, id: &str, season: &str, episode: &str) -> Option<VideoInfo> {
    let (base_url, use_id) = if id.starts_with("kitsu") {
        ("https://anime-kitsu.strem.fun/meta", id.to_string())
    } else {
        ("https://v3-cinemeta.strem.io/meta", id.to_string())
    };

    let url = format!("{}/{}/{}.json", base_url, type_, use_id);

    // Perform the HTTP request
    let client = Client::new();
    let response = client.get(&url).send();

    let response_text = match response {
        Ok(resp) => resp.text().unwrap_or_default(),
        Err(_) => return None, // Return None if the request fails
    };

    // Default values
    let mut video_info = VideoInfo {
        poster: "default_poster_url".to_string(),
        name: "Unknown Title".to_string(),
        year: "".to_string(),
        thumbnail: "default_thumbnail_url".to_string(),
        epname: "Unknown Episode Name".to_string(),
    };

    // Parse the JSON response
    let json: Value = serde_json::from_str(&response_text).unwrap_or_default();
    if !json.is_object() {
        return None; // Return None if JSON parsing fails
    }

    if let Some(meta) = json.get("meta") {
        if let Some(poster) = meta.get("poster").and_then(|p| p.as_str()) {
            video_info.poster = poster.to_string();
        }
        if let Some(name) = meta.get("name").and_then(|n| n.as_str()) {
            video_info.name = name.to_string();
        }
        if let Some(year) = meta.get("year").and_then(|y| y.as_str()) {
            video_info.year = year.to_string();
        }

        if type_ == "series" {
            if let Some(videos) = meta.get("videos").and_then(|v| v.as_array()) {
                for video in videos {
                    if let Some(video_id) = video.get("id").and_then(|v| v.as_str()) {
                        let expected_id = if season.is_empty() {
                            format!("{}:{}", id, episode)
                        } else {
                            format!("{}:{}:{}", id, season, episode)
                        };

                        if video_id == expected_id {
                            if let Some(thumbnail) = video.get("thumbnail").and_then(|t| t.as_str())
                            {
                                video_info.thumbnail = thumbnail.to_string();
                            }
                            if let Some(epname) = video.get("name").and_then(|e| e.as_str()) {
                                video_info.epname = epname.to_string();
                            } else if let Some(title) = video.get("title").and_then(|t| t.as_str())
                            {
                                video_info.epname = title.to_string();
                            } else if let Some(name) = meta.get("name").and_then(|n| n.as_str()) {
                                video_info.epname = name.to_string();
                            }
                            break;
                        }
                    }
                }
            }
        }
    }

    Some(video_info)
}

fn parse_video_id(video_id: &str) -> (String, String, String, String) {
    let parts: Vec<&str> = video_id.split(':').collect();
    if parts.first() == Some(&"kitsu") {
        match parts.len() {
            2 => (
                "movie".to_string(),
                format!("{}:{}", parts[0], parts[1]),
                "".to_string(),
                "".to_string(),
            ),
            3 => (
                "series".to_string(),
                format!("{}:{}", parts[0], parts[1]),
                "".to_string(),
                parts[2].to_string(),
            ),
            _ => (
                "unknown".to_string(),
                "".to_string(),
                "".to_string(),
                "".to_string(),
            ),
        }
    } else {
        match parts.len() {
            1 => (
                "movie".to_string(),
                parts[0].to_string(),
                "".to_string(),
                "".to_string(),
            ),
            3 => (
                "series".to_string(),
                parts[0].to_string(),
                parts[1].to_string(),
                parts[2].to_string(),
            ), // Series case
            _ => (
                "unknown".to_string(),
                "".to_string(),
                "".to_string(),
                "".to_string(),
            ),
        }
    }
}

fn run_souvlaki_media_keys(
    hwnd: *mut c_void,
    player_channel: Option<(Sender<String>, Receiver<String>)>,
) {
    // If the channel is absent, bail out
    let (player_tx, _player_rx) = match player_channel {
        Some(pair) => pair,
        None => return,
    };

    if hwnd.is_null() {
        eprintln!("Warning: HWND is null, media keys may not work properly");
    }

    let mut controls = MediaControls::new(PlatformConfig {
        dbus_name: "stremio",
        display_name: "Stremio",
        hwnd: Some(hwnd),
    })
    .expect("Cannot create MediaControls");

    controls
        .attach(move |event: MediaControlEvent| {
            eprintln!("Souvlaki event: {:?}", event);
            match event {
                MediaControlEvent::Play | MediaControlEvent::Pause => {
                    let _ = player_tx.send(r#"["mpv-command", ["cycle", "pause"]]"#.to_string());
                }
                MediaControlEvent::Next => {
                    let _ = player_tx
                        .send(r#"["mpv-command", ["seek", "10", "relative"]]"#.to_string());
                }
                MediaControlEvent::Previous => {
                    let _ = player_tx
                        .send(r#"["mpv-command", ["seek", "-10", "relative"]]"#.to_string());
                }
                MediaControlEvent::Stop => {
                    // Untested
                    let _ = player_tx.send(r#"["mpv-command", ["stop"]]"#.to_string());
                }
                MediaControlEvent::SetPosition(pos) => {
                    let _ = player_tx.send(format!(
                        r#"["mpv-command", ["seek", "{}", "absolute"]]"#,
                        pos.0.as_secs_f64()
                    ));
                }
                _ => {}
            }
        })
        .expect("Cannot attach media key callback");

    let mut last_title = String::new();
    let mut last_album = String::new();
    let mut last_cover_url = String::new();
    let mut last_total_duration = 0.0;
    let mut last_current_time = 0.0;
    let mut last_is_paused = true;

    loop {
        let current_time = *CURRENT_TIME.lock().unwrap();
        let total_duration = *TOTAL_DURATION.lock().unwrap();
        let is_paused = *IS_PAUSED.lock().unwrap();
        let title = VIDEO_TITLE.lock().unwrap().clone();
        let album_value = ALBUM.lock().unwrap().clone();
        let cover_url_value = COVER_URL.lock().unwrap().clone();

        // Detect if metadata has changed
        let metadata_changed = title != last_title
            || album_value != last_album
            || cover_url_value != last_cover_url
            || total_duration != last_total_duration;

        // Detect if playback state has changed
        let playback_changed = current_time != last_current_time || is_paused != last_is_paused;

        if metadata_changed {
            let metadata = MediaMetadata {
                title: Some(&title),
                duration: Some(Duration::from_secs_f64(total_duration)),
                album: Some(album_value.as_str()),
                cover_url: Some(cover_url_value.as_str()),
                ..Default::default()
            };

            controls.set_metadata(metadata).ok();

            // Update last known metadata values
            last_title = title;
            last_album = album_value;
            last_cover_url = cover_url_value;
            last_total_duration = total_duration;
        }

        if playback_changed {
            let progress = Some(MediaPosition(Duration::from_secs_f64(current_time)));

            if is_paused {
                controls
                    .set_playback(MediaPlayback::Paused { progress })
                    .ok();
            } else {
                controls
                    .set_playback(MediaPlayback::Playing { progress })
                    .ok();
            }

            // Update last known playback values
            last_current_time = current_time;
            last_is_paused = is_paused;
        }

        // Sleep a bit (e.g., 1 second) before updating again
        thread::sleep(Duration::from_secs(1));
    }
}

pub fn spawn_discordrpc_loop(
    app_start_time: SystemTime,
    _auto_host_lobby: bool,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let config = load_or_create_config();
        let retry_count = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));

        loop {
            let current_retry = retry_count.clone();
            let result = catch_unwind(AssertUnwindSafe(|| {
                let mut drp = DiscordIpcClient::new("997798118185771059");

                loop {
                    // Connection maintenance loop
                    // Attempt connection
                    match drp.connect() {
                        Ok(_) => {
                            current_retry.store(0, std::sync::atomic::Ordering::SeqCst);
                            println!("✅ Connected to Discord IPC");
                        }
                        Err(e) => {
                            eprintln!("⚠️ Connection failed: {e}");
                            thread::sleep(Duration::from_secs(5));
                            continue;
                        }
                    }

                    let mut last_url = String::new();
                    let mut video_info: Option<VideoInfo> = None;
                    let mut type_ = String::new();
                    let mut season = String::new();
                    let mut episode = String::new();

                    loop {
                        // Activity update loop
                        let sleep_time = Duration::from_secs(config.refresh_interval);
                        thread::sleep(sleep_time);

                        // Safely get current state with error handling
                        let (cur_url, cur_time, is_paused, total_duration) = match (
                            CURRENT_URL.lock(),
                            CURRENT_TIME.lock(),
                            IS_PAUSED.lock(),
                            TOTAL_DURATION.lock(),
                        ) {
                            (Ok(url), Ok(time), Ok(paused), Ok(duration)) => {
                                (url.clone(), *time, *paused, *duration)
                            }
                            _ => {
                                eprintln!("⚠️ Failed to lock state mutexes");
                                continue;
                            }
                        };

                        // Always send activity update (heartbeat)
                        let is_player = cur_url.contains("/player/");
                        let is_detail = cur_url.contains("/detail/");

                        // Always send activity update (heartbeat)
                        let activity_result = if is_player || is_detail {
                            // Content handling
                            if cur_url != last_url {
                                type_ = String::new();
                                season = String::new();
                                episode = String::new();

                                if is_player {
                                    let video_id = match decode(
                                        cur_url.split('/').next_back().unwrap_or(""),
                                    ) {
                                        Ok(decoded) => decoded,
                                        Err(e) => {
                                            eprintln!("⚠️ URL decoding failed: {e}");
                                            continue;
                                        }
                                    };

                                    let (parsed_type, parsed_id, parsed_season, parsed_episode) =
                                        parse_video_id(&video_id);
                                    type_ = parsed_type;
                                    season = parsed_season;
                                    episode = parsed_episode;

                                    video_info = getvidinfo(&type_, &parsed_id, &season, &episode);
                                } else if let Some(detail_part) = cur_url.split("/detail/").nth(1) {
                                    let parts: Vec<&str> = detail_part.split('/').collect();
                                    if parts.len() >= 2 {
                                        type_ = parts[0].to_string();
                                        let id = parts[1];
                                        video_info = getvidinfo(&type_, id, "", "");
                                    }
                                }
                                last_url = cur_url.clone();
                            }

                            match &video_info {
                                Some(info) => {
                                    if is_player {
                                        if config.disable_when_paused && is_paused {
                                            drp.clear_activity().map_err(|e| {
                                                Box::new(e) as Box<dyn std::error::Error>
                                            })
                                        } else {
                                            build_player_activity(
                                                &mut drp,
                                                &config,
                                                info,
                                                &type_,
                                                &season,
                                                &episode,
                                                cur_time,
                                                total_duration,
                                                is_paused,
                                                app_start_time,
                                                &cur_url,
                                            )
                                        }
                                    } else {
                                        build_detail_activity(
                                            &mut drp,
                                            &config,
                                            info,
                                            &type_,
                                            &cur_url,
                                            app_start_time,
                                        )
                                    }
                                }
                                None => {
                                    eprintln!("⚠️ No video info available");
                                    continue;
                                }
                            }
                        } else {
                            // Non-player state handling
                            if config.disable_in_menu {
                                drp.clear_activity()
                                    .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
                            } else {
                                build_menu_activity(&mut drp, &cur_url, app_start_time)
                            }
                        };

                        if let Err(e) = activity_result {
                            eprintln!("⚠️ Activity update failed: {e}");
                            let _ = drp.close();
                            break;
                        }

                        // Update metadata for media controls
                        if let Some(info) = &video_info {
                            let mut cover_guard = match COVER_URL.lock() {
                                Ok(guard) => guard,
                                Err(_) => continue,
                            };
                            *cover_guard = info.poster.clone();

                            let mut title_guard = match VIDEO_TITLE.lock() {
                                Ok(guard) => guard,
                                Err(_) => continue,
                            };
                            *title_guard = if type_ == "series" && !season.is_empty() {
                                format!("{} (S{}E{})", info.epname, season, episode)
                            } else {
                                info.name.clone()
                            };

                            let mut album_guard = match ALBUM.lock() {
                                Ok(guard) => guard,
                                Err(_) => continue,
                            };
                            *album_guard = if type_ == "series" {
                                info.thumbnail.clone()
                            } else {
                                ICON_URL.to_string()
                            };
                        }
                    } // End activity update loop

                    let _ = drp.close();
                } // End connection loop
            }));

            // Handle panics and connection failures
            if let Err(e) = result {
                eprintln!("⚠️ Critical error in Discord RPC: {:?}", e);
            }

            // Exponential backoff for reconnections
            let rc = retry_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let delay_secs = 5 * 2u64.pow(rc.min(5));
            thread::sleep(Duration::from_secs(delay_secs));
        }
    })
}

#[allow(clippy::too_many_arguments)]
fn build_player_activity(
    drp: &mut DiscordIpcClient,
    config: &Config,
    info: &VideoInfo,
    media_type: &str,
    season: &str,
    episode: &str,
    current_time: f64,
    total_duration: f64,
    is_paused: bool,
    app_start_time: SystemTime,
    cur_url: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let now_unix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;

    let (start, end) = if is_paused {
        let start_time = if config.disable_when_paused {
            app_start_time.duration_since(UNIX_EPOCH)?.as_secs() as i64
        } else {
            now_unix - current_time as i64
        };
        (start_time, None)
    } else {
        let start = now_unix - current_time as i64;
        (start, Some(start + total_duration as i64))
    };

    let mut timestamps = Timestamps::new().start(start);
    if let Some(end) = end {
        timestamps = timestamps.end(end);
    }

    let lobby = lobby_presence(config);
    let lobby_text = lobby.as_ref().and_then(LobbyPresence::others_text);

    let (mut activity_name, mut details, mut state_text) = if media_type == "series" {
        (
            info.name.clone(),
            info.epname.clone(),
            format!("S{}E{}", season, episode),
        )
    } else {
        (info.name.clone(), info.name.clone(), info.year.clone())
    };
    if config.swap_name_and_title {
        std::mem::swap(&mut activity_name, &mut details);
    }
    if let Some(text) = &lobby_text {
        state_text = if state_text.is_empty() {
            text.clone()
        } else {
            format!("{state_text} • {text}")
        };
    }

    let large_text = format!("{} ({})", info.name, info.year);
    let poster_url = weserv_contain(&info.poster);
    let mut assets = Assets::new()
        .large_image(&poster_url)
        .large_text(&large_text);

    if config.show_small_image {
        let (small_image, small_text) = if is_paused {
            (
                "https://i.imgur.com/eCUJpm9.png", // Paused icon
                "Paused",
            )
        } else {
            (ICON_URL, "Playing")
        };

        assets = assets.small_image(small_image).small_text(small_text);
    }
    // Create activity without buttons first
    let mut activity = Activity::new()
        .activity_type(ActivityType::Watching)
        .name(&activity_name)
        .details(&details)
        .state(&state_text)
        .timestamps(timestamps)
        .assets(assets);

    let last_segment = if cur_url.contains("/series/") {
        cur_url
            .split_once("/series/")
            .map(|(_, part)| format!("/series/{}", part))
    } else if cur_url.contains("/movie/") {
        cur_url
            .split_once("/movie/")
            .map(|(_, part)| format!("/movie/{}", part))
    } else {
        None
    }
    .unwrap_or_default();

    let trimmed_segment = last_segment
        .trim_start_matches("/series/")
        .trim_start_matches("/movie/");
    let raw_id = trimmed_segment.split('/').next().unwrap_or("");
    let content_id_cow = decode(raw_id).unwrap_or(std::borrow::Cow::Borrowed(raw_id));
    let content_id = content_id_cow.as_ref();

    // Add buttons if needed (using string references)
    let (external_url, stremio_url, button_label) = if config.show_buttons {
        let (label, url) = if content_id.starts_with("kitsu:") {
            let id_part = content_id.trim_start_matches("kitsu:");
            ("Kitsu", format!("https://kitsu.app/anime/{}", id_part))
        } else {
            ("IMDb", format!("https://www.imdb.com/title/{}", content_id))
        };

        let stremio = if config.link_target == "web" {
            format!("https://web.stremio.com/#/detail{}", last_segment)
        } else {
            format!("stremio:///detail{}", last_segment)
        };
        (Some(url), Some(stremio), label)
    } else {
        (None, None, "")
    };

    // ── Build Buttons ──
    // Discord allows up to 2 buttons.
    let mut buttons = Vec::new();

    // 1. External Link (IMDb/Kitsu)
    if config.show_buttons {
        if let Some(external) = external_url {
            buttons.push(Button::new(button_label, external));
        }
    }

    // 2. Open in Stremio / Join Watch Party
    if let Some(lobby) = &lobby {
        if !lobby.join_secret.is_empty() {
            buttons.push(Button::new("Join Watch Party", &lobby.join_secret));
        }
    } else if config.show_buttons {
        if let Some(stremio) = stremio_url {
            buttons.push(Button::new("Open in Stremio", stremio));
        }
    }

    if !buttons.is_empty() {
        activity = activity.buttons(buttons);
    }

    if let Some(lobby) = &lobby {
        activity = activity.party(
            Party::new()
                .id(&lobby.party_id)
                .size([lobby.member_count, lobby.max_size]),
        );
    }

    drp.set_activity(activity)?;
    Ok(())
}

fn build_menu_activity(
    drp: &mut DiscordIpcClient,
    cur_url: &str,
    app_start_time: SystemTime,
) -> Result<(), Box<dyn std::error::Error>> {
    let start_time = app_start_time.duration_since(UNIX_EPOCH)?.as_secs() as i64;

    let base_url = cur_url.split('?').next().unwrap_or(cur_url);
    let (state, details) = if base_url.ends_with("/settings") {
        ("Settings", "Changing configuration")
    } else if base_url.ends_with("/addons") {
        ("Addons", "Managing addons")
    } else if base_url.ends_with("/library") {
        ("Library", "Browsing library")
    } else if base_url.ends_with("/calendar") {
        ("Calendar", "Viewing Calendar")
    } else if base_url.ends_with("/discover") {
        ("Discover", "Browsing Catalog")
    } else {
        ("Browsing", "In Stremio Menu")
    };

    let activity = Activity::new()
        .activity_type(ActivityType::Watching)
        .name("Stremio")
        .details(details)
        .state(state)
        .timestamps(Timestamps::new().start(start_time))
        .assets(Assets::new().large_image(ICON_URL).large_text("Stremio"));

    drp.set_activity(activity)?;
    Ok(())
}

impl MainWindow {
    fn transmit_window_visibility_change(&self) {
        let Ok(web_channel) = self.webview.channel.try_borrow() else {
            return;
        };
        let Ok(style) = self.saved_window_style.try_borrow() else {
            return;
        };
        let Some((web_tx, _)) = web_channel.as_ref() else {
            return;
        };

        let web_tx_app = web_tx.clone();
        web_tx_app
            .send(RPCResponse::visibility_change(
                self.window.visible(),
                style.full_screen as u32,
                style.full_screen,
            ))
            .ok();
    }
    fn transmit_window_state_change(&self) {
        let Some(hwnd) = self.window.handle.hwnd() else {
            return;
        };
        let Ok(web_channel) = self.webview.channel.try_borrow() else {
            return;
        };
        let Ok(style) = self.saved_window_style.try_borrow() else {
            return;
        };
        let Some((web_tx, _)) = web_channel.as_ref() else {
            return;
        };

        let state = style.clone().get_window_state(hwnd);
        let web_tx_app = web_tx.clone();
        web_tx_app.send(RPCResponse::state_change(state)).ok();
    }
    fn transmit_pip_change(&self, enabled: bool) {
        if let Ok(web_channel) = self.webview.channel.try_borrow() {
            if let Some((web_tx, _)) = web_channel.as_ref() {
                web_tx.clone().send(RPCResponse::pip_change(enabled)).ok();
            }
        }
    }
    fn on_init(&self) {
        let webui_url =
            if self.webui_url.trim_end_matches('/') == WEB_ENDPOINT.trim_end_matches('/') {
                self.server
                    .server_url()
                    .map(|server_url| web_endpoint_with_streaming_server(&server_url))
                    .unwrap_or_else(|| self.webui_url.clone())
            } else {
                self.webui_url.clone()
            };
        self.webview.endpoint.set(webui_url).ok();
        self.webview.dev_tools.set(self.dev_tools).ok();
        if let Some(hwnd) = self.window.handle.hwnd() {
            // When MPV is detached into PiP, the transparent WebView can reveal
            // the main window background during resize. Black matches video
            // letterboxing and avoids white flashes.
            nwg::bind_raw_event_handler(&self.window.handle, 0x13000, move |hwnd, msg, w, _l| {
                if msg == WM_ERASEBKGND {
                    unsafe {
                        let mut rect: RECT = std::mem::zeroed();
                        GetClientRect(hwnd, &mut rect);
                        FillRect(
                            w as HDC,
                            &rect,
                            GetStockObject(BLACK_BRUSH as i32) as HBRUSH,
                        );
                    }
                    return Some(1);
                }
                None
            })
            .ok();

            let player_channel = self.player.channel.borrow().clone();
            let safe_hwnd = SafeHwnd(hwnd as *mut c_void);

            std::thread::spawn(move || {
                run_souvlaki_media_keys(safe_hwnd.0, player_channel);
            });

            if let Ok(mut saved_style) = self.saved_window_style.try_borrow_mut() {
                saved_style.set_title_bar_color(hwnd);
                if let Some(window_settings) = WindowSettings::load() {
                    saved_style
                        .restore_window_placement(hwnd, window_settings.to_window_placement());
                } else {
                    saved_style.center_window(hwnd, WINDOW_MIN_WIDTH, WINDOW_MIN_HEIGHT);
                }
            }

            // Make the title bar follow the Windows dark/light theme and
            // use Mica backdrop on Windows 11 for a modern translucent look.
            unsafe {
                use winapi::shared::minwindef::{DWORD, HKEY};
                use winapi::um::dwmapi::{DwmExtendFrameIntoClientArea, DwmSetWindowAttribute};
                use winapi::um::uxtheme::MARGINS;
                use winapi::um::winnt::KEY_READ;
                use winapi::um::winreg::{
                    RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY_CURRENT_USER,
                };

                // Read the system dark/light mode from the registry
                let mut is_dark = true; // default to dark
                let subkey: Vec<u16> =
                    "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize\0"
                        .encode_utf16()
                        .collect();
                let value_name: Vec<u16> = "AppsUseLightTheme\0".encode_utf16().collect();
                let mut hkey: HKEY = std::ptr::null_mut();
                if RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_READ, &mut hkey) == 0 {
                    let mut data: DWORD = 0;
                    let mut data_size: DWORD = std::mem::size_of::<DWORD>() as DWORD;
                    if RegQueryValueExW(
                        hkey,
                        value_name.as_ptr(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        &mut data as *mut DWORD as *mut u8,
                        &mut data_size,
                    ) == 0
                    {
                        // AppsUseLightTheme: 0 = dark, 1 = light
                        is_dark = data == 0;
                    }
                    RegCloseKey(hkey);
                }

                // DWMWA_USE_IMMERSIVE_DARK_MODE (attribute 20)
                const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
                let dark_mode: i32 = if is_dark { 1 } else { 0 };
                let hr = DwmSetWindowAttribute(
                    hwnd,
                    DWMWA_USE_IMMERSIVE_DARK_MODE,
                    &dark_mode as *const i32 as *const _,
                    std::mem::size_of::<i32>() as u32,
                );
                if hr != 0 {
                    eprintln!(
                        "[DWM] DwmSetWindowAttribute(IMMERSIVE_DARK_MODE) failed: 0x{:08X}",
                        hr
                    );
                }

                // DWMWA_SYSTEMBACKDROP_TYPE (attribute 38) — Windows 11 22H2+
                // Value 2 = Mica, 3 = Acrylic, 4 = Mica Alt
                const DWMWA_SYSTEMBACKDROP_TYPE: u32 = 38;
                let backdrop_type: i32 = 2; // Mica
                let hr = DwmSetWindowAttribute(
                    hwnd,
                    DWMWA_SYSTEMBACKDROP_TYPE,
                    &backdrop_type as *const i32 as *const _,
                    std::mem::size_of::<i32>() as u32,
                );
                if hr != 0 {
                    eprintln!(
                        "[DWM] DwmSetWindowAttribute(SYSTEMBACKDROP_TYPE) failed: 0x{:08X}",
                        hr
                    );
                }

                // Extend the frame into the client area — required for Mica
                // to be visible on Win32 windows. A top margin of -1 extends
                // the frame (and Mica effect) fully.
                let margins = MARGINS {
                    cxLeftWidth: 0,
                    cxRightWidth: 0,
                    cyTopHeight: -1,
                    cyBottomHeight: 0,
                };
                let hr = DwmExtendFrameIntoClientArea(hwnd, &margins);
                if hr != 0 {
                    eprintln!("[DWM] DwmExtendFrameIntoClientArea failed: 0x{:08X}", hr);
                }
            }
        }

        let sync_notice_sender = self.sync_notice.sender();
        let sync_events_queue = self.sync_events.clone();
        let (sync_event_tx, sync_event_rx) = flume::unbounded();
        crate::stremio_app::steam_sync::set_ui_event_sender(sync_event_tx);
        thread::spawn(move || {
            while let Ok(event) = sync_event_rx.recv() {
                if let Ok(mut events) = sync_events_queue.lock() {
                    events.push_back(event);
                }
                sync_notice_sender.notice();
            }
        });

        let player_channel = self.player.channel.borrow();
        let Some((player_tx, player_rx)) = player_channel.as_ref() else {
            nwg::modal_error_message(
                self.window.handle,
                "Stremio Startup Error",
                "Cannot initialize the player communication channel. Stremio will close.",
            );
            nwg::stop_thread_dispatch();
            return;
        };
        let player_tx = player_tx.clone();
        let player_rx = player_rx.clone();

        // Make the player command sender available globally for the sync client
        use crate::stremio_app::stremio_player::player::PLAYER_CMD_TX;
        if let Ok(mut tx_global) = PLAYER_CMD_TX.lock() {
            *tx_global = Some(player_tx.clone());
        }

        let (pip_event_tx, pip_event_rx) = flume::unbounded::<String>();
        let pip_placement = PipPlacement::load();
        if let Ok(mut pip) = self.pip_window.try_borrow_mut() {
            let ctx = PipBuildContext {
                close_sender: self.toggle_pip_notice.sender(),
                player_tx: player_tx.clone(),
                player_event_rx: pip_event_rx,
                initial_pos: pip_placement.as_ref().map(|p| (p.x, p.y)),
                initial_size: pip_placement.as_ref().map(|p| (p.width, p.height)),
                initial_transparent: pip_placement
                    .as_ref()
                    .map(|p| p.transparent)
                    .unwrap_or(false),
            };
            if let Err(err) = pip.build(ctx) {
                eprintln!("Failed to build PiP window: {err:?}");
            }
        }

        let web_channel = self.webview.channel.borrow();
        let Some((web_tx, web_rx)) = web_channel.as_ref() else {
            nwg::modal_error_message(
                self.window.handle,
                "Stremio Startup Error",
                "Cannot initialize the Web UI communication channel. Stremio will close.",
            );
            nwg::stop_thread_dispatch();
            return;
        };
        let web_tx_player = web_tx.clone();
        let web_tx_web = web_tx.clone();
        let web_tx_arg = web_tx.clone();
        let web_tx_upd = web_tx.clone();

        use crate::stremio_app::stremio_wevbiew::wevbiew::WEB_CMD_TX;
        if let Ok(mut tx_global) = WEB_CMD_TX.lock() {
            *tx_global = Some(web_tx.clone());
        }

        let web_rx = web_rx.clone();
        let (updater_tx, updater_rx) = flume::unbounded::<String>();
        let updater_tx_web = updater_tx.clone();

        let app_start_time = SystemTime::now();
        let auto_host_lobby = !self.command.starts_with("stremio://sync/");
        let config = load_or_create_config();
        spawn_discordrpc_loop(app_start_time, auto_host_lobby);
        crate::stremio_app::intro_skip::spawn_intro_skip_loop(
            crate::stremio_app::intro_skip::IntroSkipConfig {
                enabled: config.auto_skip_enabled,
                skip_intro: config.auto_skip_intro,
                skip_recap: config.auto_skip_recap,
                skip_outro: config.auto_skip_outro,
                chapter_intro_words: config.chapter_intro_words.clone(),
                chapter_recap_words: config.chapter_recap_words.clone(),
                chapter_outro_words: config.chapter_outro_words.clone(),
                introdb_api_key: config.introdb_api_key.clone(),
                theintrodb_api_key: config.theintrodb_api_key.clone(),
            },
        );
        self.update_watch_party_menu();

        self.window.set_visible(!self.start_hidden);
        self.tray.tray_show_hide.set_checked(!self.start_hidden);
        if self.no_splash {
            self.splash_screen.hide();
        }

        let command_clone = self.command.clone();

        // Single application IPC
        let socket_path = Path::new(
            self.commands_path
                .as_ref()
                .expect("Cannot initialie the single application IPC"),
        );

        let autoupdater_endpoint = self.autoupdater_endpoint.clone();
        let force_update = self.force_update;
        let release_candidate = self.release_candidate;
        let autoupdater_setup_file = self.autoupdater_setup_file.clone();

        thread::spawn(move || {
            loop {
                if let Ok(msg) = updater_rx.recv() {
                    if msg == "check_for_update" {
                        break;
                    }
                }
            }

            loop {
                let current_version = env!("CARGO_PKG_VERSION")
                    .parse()
                    .expect("Should always be valid");

                let updater_endpoint = autoupdater_endpoint
                    .clone()
                    .unwrap_or_else(|| Url::parse(UPDATE_ENDPOINT).expect("valid updater URL"));

                let updater = updater::Updater::new(
                    current_version,
                    &updater_endpoint,
                    force_update,
                    release_candidate,
                );
                match updater.autoupdate() {
                    Ok(Some(update)) => {
                        println!("New version ready to install v{}", update.version);
                        let mut autoupdater_setup_file = autoupdater_setup_file.lock().unwrap();
                        *autoupdater_setup_file = Some(update.file.clone());
                        web_tx_upd.send(RPCResponse::update_available()).ok();
                    }
                    Ok(None) => println!("No new updates found"),
                    Err(e) => eprintln!("Failed to fetch updates: {e}"),
                }

                thread::sleep(time::Duration::from_secs(UPDATE_INTERVAL));
            }
        }); // thread

        if let Ok(mut listener) = PipeServer::bind(socket_path) {
            let focus_sender = self.focus_notice.sender();
            thread::spawn(move || loop {
                if let Ok(mut stream) = listener.accept() {
                    let mut buf = vec![];
                    stream.read_to_end(&mut buf).ok();
                    if let Ok(s) = str::from_utf8(&buf) {
                        focus_sender.notice();
                        let s_str = s.to_string();
                        if s_str.starts_with("stremio://sync/") {
                            use crate::stremio_app::steam_sync;
                            match steam_sync::connect_to_host(&s_str) {
                                Ok(_) => println!("✅ Sync client connected!"),
                                Err(e) => eprintln!("⚠️ Failed to connect: {e}"),
                            }
                        } else {
                            // ['open-media', url]
                            web_tx_arg.send(RPCResponse::open_media(s_str)).ok();
                        }
                        println!("{s}");
                    }
                }
            });
        }

        // Read message from player; tee updates to the Web UI and the PiP overlay.
        thread::spawn(move || {
            for msg in player_rx.iter() {
                #[cfg(debug_assertions)]
                if msg.contains("mpv-event-ended")
                    || msg.contains("mpv-event-error")
                    || msg.contains("mpv-prop-change")
                        && (msg.contains("\"path\"")
                            || msg.contains("\"duration\"")
                            || msg.contains("\"time-pos\""))
                {
                    println!("[PLAYER->WEB] {msg}");
                }
                let _ = web_tx_player.send(msg.clone());
                let _ = pip_event_tx.send(msg);
            }
        }); // thread

        let toggle_fullscreen_sender = self.toggle_fullscreen_notice.sender();
        let toggle_pip_sender = self.toggle_pip_notice.sender();
        let quit_sender = self.quit_notice.sender();
        let hide_splash_sender = self.hide_splash_notice.sender();
        let focus_sender = self.focus_notice.sender();
        let autoupdater_setup_mutex = self.autoupdater_setup_file.clone();
        let discord_rpc = DiscordRpc::new(web_tx.clone());
        let requested_fullscreen = self.requested_fullscreen.clone();
        let requested_pip = self.requested_pip.clone();
        thread::spawn(move || loop {
            if let Some(msg) = web_rx
                .recv()
                .ok()
                .and_then(|s| serde_json::from_str::<RPCRequest>(&s).ok())
            {
                #[cfg(debug_assertions)]
                if let Some(method) = msg.get_method() {
                    if method.starts_with("mpv-")
                        || method == "shell-route-changed"
                        || method == "app-ready"
                    {
                        println!("[WEB->SHELL] method={method} params={:?}", msg.get_params());
                    }
                }
                match msg.get_method() {
                    // The handshake. Here we send some useful data to the WEB UI
                    None if msg.is_handshake() => {
                        web_tx_web.send(RPCResponse::get_handshake()).ok();
                    }
                    Some("win-set-visibility") => {
                        if let Some(fullscreen) = msg
                            .get_params()
                            .and_then(|params| params.get("fullscreen"))
                            .and_then(|value| value.as_bool())
                        {
                            *requested_fullscreen.lock().unwrap() = Some(fullscreen);
                            toggle_fullscreen_sender.notice();
                        }
                    }
                    Some("win-set-pip") => {
                        let target = msg
                            .get_params()
                            .and_then(|params| params.get("enabled"))
                            .and_then(|value| value.as_bool());
                        *requested_pip.lock().unwrap() = target;
                        toggle_pip_sender.notice();
                    }
                    Some("quit") => quit_sender.notice(),
                    Some("shell-route-changed") => {
                        if let Some(url) = msg.get_params().and_then(|arg| arg.as_str()) {
                            println!("[WEB->SHELL] route changed: {url}");
                            if let Ok(mut current_url) = CURRENT_URL.lock() {
                                *current_url = url.to_string();
                            }
                        }
                    }
                    Some("shell-watch-party-leave") => {
                        if let Err(e) = crate::stremio_app::steam_sync::leave_lobby() {
                            eprintln!("⚠️ Failed to leave watch party: {e}");
                        }
                    }
                    Some("shell-watch-party-kick") => {
                        let role = LOBBY_ROLE.lock().map(|r| *r).unwrap_or(LobbyRole::None);
                        if role == LobbyRole::Host {
                            let steam_id = msg.get_params().and_then(|arg| {
                                let value = arg.get("steamId")?;
                                value
                                    .as_str()
                                    .and_then(|id| id.parse::<u64>().ok())
                                    .or_else(|| value.as_u64())
                            });
                            if let Some(steam_id) = steam_id {
                                if let Err(e) =
                                    crate::stremio_app::steam_sync::kick_member(steam_id)
                                {
                                    eprintln!("⚠️ Failed to remove watch party member: {e}");
                                }
                            }
                        }
                    }
                    Some("app-ready") => {
                        hide_splash_sender.notice();
                        web_tx_web
                            .send(RPCResponse::visibility_change(true, 1, false))
                            .ok();
                        updater_tx_web
                            .send("check_for_update".to_owned())
                            .expect("Failed to send value to updater channel");

                        let command_ref = command_clone.clone();
                        if !command_ref.is_empty() {
                            if command_ref.starts_with("stremio://sync/") {
                                use crate::stremio_app::steam_sync;
                                match steam_sync::connect_to_host(&command_ref) {
                                    Ok(_) => println!("✅ Sync client connected!"),
                                    Err(e) => eprintln!("⚠️ Failed to connect: {e}"),
                                }
                            } else {
                                web_tx_web.send(RPCResponse::open_media(command_ref)).ok();
                            }
                        }
                    }
                    Some("app-error") => {
                        hide_splash_sender.notice();
                        if let Some(arg) = msg.get_params() {
                            // TODO: Make this modal dialog
                            eprintln!("Web App Error: {arg}");
                        }
                    }
                    Some("open-external") => {
                        if let Some(arg) = msg.get_params() {
                            // FIXME: THIS IS NOT SAFE BY ANY MEANS
                            // open::that("calc").ok(); does exactly that
                            let arg = arg.as_str().unwrap_or("");
                            let arg_lc = arg.to_lowercase();
                            if arg_lc.starts_with("http://")
                                || arg_lc.starts_with("https://")
                                || arg_lc.starts_with("rtp://")
                                || arg_lc.starts_with("rtps://")
                                || arg_lc.starts_with("ftp://")
                                || arg_lc.starts_with("ipfs://")
                            {
                                open::that(arg).ok();
                            }
                        }
                    }
                    Some("play-external") => {
                        if let Some(arg) = msg.get_params() {
                            let arg = arg.as_str().unwrap_or("");
                            let arg_lc = arg.to_lowercase();
                            const ALLOWED_SCHEMES: &[&str] = &["mpv://", "vlc://", "potplayer://"];
                            let allowed = ALLOWED_SCHEMES.iter().any(|s| arg_lc.starts_with(s));
                            if !arg.is_empty() && allowed {
                                if let Some(stream_url) =
                                    arg_lc.starts_with("mpv://").then(|| &arg[6..])
                                {
                                    // `--` ends mpv's option parsing; the stream URL can't smuggle flags.
                                    let mpv_paths: Vec<String> = vec![
                                        std::env::var("ProgramFiles")
                                            .ok()
                                            .map(|v| format!("{v}\\mpv\\mpv.exe")),
                                        std::env::var("ProgramFiles(x86)")
                                            .ok()
                                            .map(|v| format!("{v}\\mpv\\mpv.exe")),
                                        std::env::var("LOCALAPPDATA")
                                            .ok()
                                            .map(|v| format!("{v}\\Programs\\mpv\\mpv.exe")),
                                        std::env::var("LOCALAPPDATA")
                                            .ok()
                                            .map(|v| format!("{v}\\mpv\\mpv.exe")),
                                        Some("mpv.exe".to_string()),
                                    ]
                                    .into_iter()
                                    .flatten()
                                    .collect();
                                    for path in &mpv_paths {
                                        if Command::new(path)
                                            .arg("--")
                                            .arg(stream_url)
                                            .creation_flags(CREATE_BREAKAWAY_FROM_JOB)
                                            .spawn()
                                            .is_ok()
                                        {
                                            break;
                                        }
                                    }
                                } else {
                                    open::that(arg).ok();
                                }
                            }
                        }
                    }
                    Some("win-focus") => {
                        focus_sender.notice();
                    }
                    Some("autoupdater-notif-clicked") => {
                        // We've shown the "Update Available" notification
                        // and the user clicked on "Restart And Update"
                        let autoupdater_setup_file =
                            autoupdater_setup_mutex.lock().unwrap().clone();
                        match autoupdater_setup_file {
                            Some(file_path) => {
                                println!("Running the setup at {file_path:?}");

                                let command = Command::new(file_path)
                                    .args([
                                        "/SILENT",
                                        "/NOCANCEL",
                                        "/FORCECLOSEAPPLICATIONS",
                                        "/TASKS=runapp",
                                    ])
                                    .creation_flags(CREATE_BREAKAWAY_FROM_JOB)
                                    .stdin(process::Stdio::null())
                                    .stdout(process::Stdio::null())
                                    .stderr(process::Stdio::null())
                                    .spawn();

                                match command {
                                    Ok(process) => {
                                        println!("Updater started. (PID {:?})", process.id());
                                        quit_sender.notice();
                                    }
                                    Err(err) => eprintln!("Updater couldn't be started: {err}"),
                                };
                            }
                            _ => {
                                println!("Cannot obtain the setup file path");
                            }
                        }
                    }
                    Some("discord-connect") => {
                        if let Err(e) = discord_rpc.connect() {
                            eprintln!("Discord connect error: {}", e);
                            web_tx_web.send(RPCResponse::discord_status(false)).ok();
                        }
                    }
                    Some("discord-disconnect") => {
                        if let Err(e) = discord_rpc.disconnect() {
                            eprintln!("Discord disconnect error: {}", e);
                        }
                        web_tx_web.send(RPCResponse::discord_status(false)).ok();
                    }
                    Some("discord-set-activity") => {
                        if let Some(params) = msg.get_params() {
                            let state = params.get("state").and_then(|v| v.as_str()).unwrap_or("");
                            let details =
                                params.get("details").and_then(|v| v.as_str()).unwrap_or("");
                            let image = params.get("image").and_then(|v| v.as_str());
                            let start_timestamp =
                                params.get("startTimestamp").and_then(|v| v.as_i64());
                            let end_timestamp = params.get("endTimestamp").and_then(|v| v.as_i64());

                            if let Err(e) = discord_rpc.set_activity(
                                state,
                                details,
                                image,
                                start_timestamp,
                                end_timestamp,
                            ) {
                                eprintln!("Discord set activity error: {}", e);
                            }
                        }
                    }
                    Some("discord-clear-activity") => {
                        if let Err(e) = discord_rpc.clear_activity() {
                            eprintln!("Discord clear activity error: {}", e);
                        }
                    }
                    Some(player_command) if player_command.starts_with("mpv-") => {
                        let player_command = player_command.to_string();
                        let resp_json = serde_json::to_string(
                            &msg.args.expect("Cannot have method without args"),
                        )
                        .expect("Cannot build response");
                        println!("[WEB->PLAYER] {player_command}: {resp_json}");
                        if let Err(err) = player_tx.send(resp_json) {
                            eprintln!("[WEB->PLAYER] failed to send {player_command}: {err}");
                        }
                    }
                    Some("media.status") => {
                        handle_media_status(msg.get_params());
                    }
                    Some("media.metadata") => {
                        handle_media_metadata(msg.get_params());
                    }
                    Some(unknown) => {
                        eprintln!("Unsupported command {}({:?})", unknown, msg.get_params())
                    }
                    None => {}
                }
            } // recv
        }); // thread
    }
    fn on_min_max(&self, data: &nwg::EventData) {
        let data = data.on_min_max();
        data.set_min_size(WINDOW_MIN_WIDTH, WINDOW_MIN_HEIGHT);
    }
    fn on_paint(&self) {
        if !self.splash_screen.visible() {
            self.webview.fit_to_window(self.window.handle.hwnd());
        }
    }
    fn on_window_state_changed(&self) {
        self.save_window_settings();
        self.transmit_window_state_change();
    }
    fn save_window_settings(&self) {
        if self
            .saved_window_style
            .try_borrow()
            .map(|style| style.full_screen)
            .unwrap_or(false)
        {
            return;
        }
        if let Some(hwnd) = self.window.handle.hwnd() {
            if let Err(err) = WindowSettings::save(hwnd) {
                eprintln!("Cannot save window settings: {err}");
            }
        }
    }
    fn on_toggle_fullscreen_notice(&self) {
        if self.pip_active.get() {
            *self.requested_pip.lock().unwrap() = Some(false);
            self.on_toggle_pip_notice();
        }
        if let Some(hwnd) = self.window.handle.hwnd() {
            if let Ok(mut saved_style) = self.saved_window_style.try_borrow_mut() {
                let target = self
                    .requested_fullscreen
                    .lock()
                    .unwrap()
                    .take()
                    .unwrap_or(!saved_style.full_screen);
                saved_style.set_full_screen(hwnd, target);
                self.tray.tray_topmost.set_enabled(!saved_style.full_screen);
                self.tray
                    .tray_topmost
                    .set_checked((saved_style.ex_style as u32 & WS_EX_TOPMOST) == WS_EX_TOPMOST);
            }
        }
        self.transmit_window_visibility_change();
    }
    fn on_tray_toggle_pip(&self) {
        *self.requested_pip.lock().unwrap() = None;
        self.toggle_pip_notice.sender().notice();
    }
    fn on_toggle_pip_notice(&self) {
        let Some(main_hwnd) = self.window.handle.hwnd() else {
            return;
        };

        if self
            .saved_window_style
            .try_borrow()
            .map(|style| style.full_screen)
            .unwrap_or(false)
        {
            self.requested_pip.lock().unwrap().take();
            return;
        }

        let requested = self.requested_pip.lock().unwrap().take();
        let target = requested.unwrap_or(!self.pip_active.get());
        if target == self.pip_active.get() {
            return;
        }

        let pip_ref = match self.pip_window.try_borrow() {
            Ok(pip) => pip,
            Err(_) => return,
        };
        if !pip_ref.built.get() {
            return;
        }

        if target {
            let mpv_child = self
                .mpv_child_hwnd
                .get()
                .or_else(|| find_mpv_child_hwnd(main_hwnd));
            let Some(child) = mpv_child else {
                eprintln!("PiP enable: MPV child window not found yet");
                return;
            };
            self.mpv_child_hwnd.set(Some(child));
            pip_ref.attach_video(child);
            pip_ref.show();
            self.pip_active.set(true);
            self.tray.tray_pip.set_checked(true);
            self.transmit_pip_change(true);
        } else {
            self.save_pip_placement();
            pip_ref.detach_video(main_hwnd);
            pip_ref.hide();
            self.pip_active.set(false);
            self.tray.tray_pip.set_checked(false);
            self.transmit_pip_change(false);
            self.webview.fit_to_window(Some(main_hwnd));
        }
    }
    fn save_pip_placement(&self) {
        let Ok(pip_ref) = self.pip_window.try_borrow() else {
            return;
        };
        let Some((x, y, width, height)) = pip_ref.current_placement() else {
            return;
        };
        let _ = PipPlacement::save(PipPlacement {
            x,
            y,
            width,
            height,
            transparent: pip_ref.transparency_enabled(),
        });
    }
    fn on_hide_splash_notice(&self) {
        self.splash_screen.hide();
    }
    fn on_focus_notice(&self) {
        self.window.set_visible(true);
        if let Some(hwnd) = self.window.handle.hwnd() {
            if let Ok(mut saved_style) = self.saved_window_style.try_borrow_mut() {
                saved_style.set_active(hwnd);
            }
        }
    }
    fn on_sync_notice(&self) {
        let events = if let Ok(mut queue) = self.sync_events.lock() {
            queue.drain(..).collect::<Vec<_>>()
        } else {
            Vec::new()
        };

        for event in events {
            self.show_sync_event(event);
        }
        self.update_watch_party_menu();
    }
    fn show_sync_event(&self, event: crate::stremio_app::steam_sync::SyncUiEvent) {
        use crate::stremio_app::steam_sync::SyncUiEvent;

        let (title, text, flags, show_dialog) = match event {
            SyncUiEvent::HostStarted => (
                "Watch Party",
                "Your watch party is ready. Friends can join from Discord.".to_string(),
                nwg::TrayNotificationFlags::INFO_ICON,
                false,
            ),
            SyncUiEvent::JoinedHost => (
                "Watch Party",
                "Joined the watch party. Playback will sync with the host.".to_string(),
                nwg::TrayNotificationFlags::INFO_ICON,
                false,
            ),
            SyncUiEvent::LobbyUpdated {
                member_count,
                max_size,
            } => (
                "Watch Party",
                format!("Watch party {member_count}/{max_size}"),
                nwg::TrayNotificationFlags::INFO_ICON,
                false,
            ),
            SyncUiEvent::GuestJoined { name, member_count } => (
                "Watch Party",
                format!("{name} joined. {member_count} people are in the watch party."),
                nwg::TrayNotificationFlags::INFO_ICON,
                false,
            ),
            SyncUiEvent::GuestLeft { name, member_count } => (
                "Watch Party",
                format!("{name} left. {member_count} people remain in the watch party."),
                nwg::TrayNotificationFlags::INFO_ICON,
                false,
            ),
            SyncUiEvent::HostLeft { reason } => (
                "Watch Party Ended",
                reason,
                nwg::TrayNotificationFlags::WARNING_ICON,
                false,
            ),
            SyncUiEvent::LeftLobby => (
                "Watch Party",
                "Left the watch party.".to_string(),
                nwg::TrayNotificationFlags::INFO_ICON,
                false,
            ),
            SyncUiEvent::Error { message } => (
                "Watch Party Error",
                message,
                nwg::TrayNotificationFlags::ERROR_ICON,
                true,
            ),
        };

        println!("Watch party UI event: {title}: {text}");
        self.tray.tray.show(&text, Some(title), Some(flags), None);
        self.flash_watch_party_notice();
        self.update_watch_party_menu();
        self.update_watch_party_overlay(Some(title), Some(&text));
        if show_dialog {
            nwg::modal_error_message(self.window.handle, title, &text);
        }
    }
    fn flash_watch_party_notice(&self) {
        if let Some(hwnd) = self.window.handle.hwnd() {
            unsafe {
                use winapi::um::winuser::{
                    FlashWindowEx, FLASHWINFO, FLASHW_TIMERNOFG, FLASHW_TRAY,
                };

                let mut info = FLASHWINFO {
                    cbSize: std::mem::size_of::<FLASHWINFO>() as u32,
                    hwnd,
                    dwFlags: FLASHW_TRAY | FLASHW_TIMERNOFG,
                    uCount: 3,
                    dwTimeout: 0,
                };
                FlashWindowEx(&mut info);
            }
        }
    }
    fn update_watch_party_menu(&self) {
        let party_id = LOBBY_PARTY_ID.lock().map(|p| p.clone()).unwrap_or_default();
        let member_count = LOBBY_MEMBER_COUNT.lock().map(|c| *c).unwrap_or(0);
        let max_size = LOBBY_MAX_SIZE.lock().map(|m| *m).unwrap_or(8);
        let role = LOBBY_ROLE.lock().map(|r| *r).unwrap_or(LobbyRole::None);

        if party_id.is_empty() || member_count <= 0 {
            self.window.set_text(APP_NAME);
            self.tray.tray_start_watch_party.set_enabled(true);
            self.tray.tray_end_watch_party.set_enabled(false);
            self.tray.tray_leave_watch_party.set_enabled(false);
            self.tray.tray.set_tip("Stremio");
        } else {
            let role_label = match role {
                LobbyRole::Host => "Hosting",
                LobbyRole::Guest => "Joined",
                LobbyRole::None => "Watch party",
            };
            let party_text = format!("{role_label} watch party {member_count}/{max_size}");

            self.window.set_text(&format!("{APP_NAME} - {party_text}"));
            self.tray.tray_start_watch_party.set_enabled(false);
            self.tray
                .tray_end_watch_party
                .set_enabled(role == LobbyRole::Host);
            self.tray
                .tray_leave_watch_party
                .set_enabled(role != LobbyRole::Host);
            self.tray
                .tray
                .set_tip(&format!("Stremio - Watch party {member_count}/{max_size}"));
        }
        self.update_watch_party_overlay(None, None);
    }
    fn on_start_watch_party(&self) {
        let party_id = LOBBY_PARTY_ID.lock().map(|p| p.clone()).unwrap_or_default();
        if !party_id.is_empty() {
            self.update_watch_party_menu();
            return;
        }

        let config = load_or_create_config();
        let party_id = format!("stremio-{}", uuid::Uuid::new_v4());
        match crate::stremio_app::steam_sync::start_host_lobby(
            party_id.clone(),
            config.lobby_max_size,
        ) {
            Ok(join_secret) => {
                if let Ok(mut pid) = LOBBY_PARTY_ID.lock() {
                    *pid = party_id;
                }
                if let Ok(mut secret) = LOBBY_JOIN_SECRET.lock() {
                    *secret = join_secret;
                }
                if let Ok(mut cnt) = LOBBY_MEMBER_COUNT.lock() {
                    *cnt = 1;
                }
                if let Ok(mut max) = LOBBY_MAX_SIZE.lock() {
                    *max = config.lobby_max_size;
                }
                self.update_watch_party_menu();
                self.update_watch_party_overlay(
                    Some("Watch Party"),
                    Some("Your watch party is ready. Friends can join from Discord."),
                );
            }
            Err(e) => {
                self.tray.tray.show(
                    &format!("Could not start watch party: {e}"),
                    Some("Watch Party Error"),
                    Some(nwg::TrayNotificationFlags::ERROR_ICON),
                    None,
                );
                self.update_watch_party_overlay(Some("Watch Party Error"), Some(&e));
                self.update_watch_party_menu();
            }
        }
    }
    fn on_end_watch_party(&self) {
        let role = LOBBY_ROLE.lock().map(|r| *r).unwrap_or(LobbyRole::None);
        if role != LobbyRole::Host {
            self.update_watch_party_menu();
            return;
        }

        if let Err(e) = crate::stremio_app::steam_sync::leave_lobby() {
            self.tray.tray.show(
                &format!("Could not end watch party: {e}"),
                Some("Watch Party Error"),
                Some(nwg::TrayNotificationFlags::ERROR_ICON),
                None,
            );
        }
        self.update_watch_party_menu();
    }
    fn update_watch_party_overlay(&self, event_title: Option<&str>, event_text: Option<&str>) {
        let party_id = LOBBY_PARTY_ID.lock().map(|p| p.clone()).unwrap_or_default();
        let member_count = LOBBY_MEMBER_COUNT.lock().map(|c| *c).unwrap_or(0);
        let max_size = LOBBY_MAX_SIZE.lock().map(|m| *m).unwrap_or(8);
        let members = LOBBY_MEMBERS.lock().map(|m| m.clone()).unwrap_or_default();
        let role = LOBBY_ROLE.lock().map(|r| *r).unwrap_or(LobbyRole::None);
        let active = !party_id.is_empty() && member_count > 0;
        let role_label = match role {
            LobbyRole::Host => "Hosting",
            LobbyRole::Guest => "Joined",
            LobbyRole::None => "Watch party",
        };
        let can_leave = active && role != LobbyRole::Host;
        let badge = if active {
            format!("{role_label} watch party {member_count}/{max_size}")
        } else if let Some(title) = event_title {
            title.to_string()
        } else {
            String::new()
        };
        let visible = active || event_title.is_some() || event_text.is_some();

        let payload = serde_json::json!({
            "active": active,
            "visible": visible,
            "badge": badge,
            "canLeave": can_leave,
            "canKick": active && role == LobbyRole::Host,
            "members": members.iter().map(|member| {
                serde_json::json!({
                    "steamId": member.steam_id.to_string(),
                    "name": member.name,
                    "isHost": member.is_host,
                })
            }).collect::<Vec<_>>(),
            "eventTitle": event_title,
            "eventText": event_text,
        });
        let script = r##"(function() {
                const state = __STREMIO_WATCH_PARTY_STATE__;
                const id = "stremio-shell-watch-party";
                let el = document.getElementById(id);
                if (!state.visible) {
                    if (el) el.remove();
                    return;
                }
                if (!el) {
                    el = document.createElement("div");
                    el.id = id;
                    document.documentElement.appendChild(el);
                }
                if (!el.querySelector('[data-role="panel"]')) {
                    el.style.cssText = [
                        "position:fixed",
                        "left:50%",
                        "top:0",
                        "transform:translateX(-50%)",
                        "z-index:2147483647",
                        "width:min(420px,calc(100vw - 40px))",
                        "height:142px",
                        "pointer-events:none"
                    ].join(";");
                    el.innerHTML = `
                        <div data-role="hotspot" style="position:absolute;left:50%;top:0;transform:translateX(-50%);width:270px;height:30px;pointer-events:auto"></div>
                        <div data-role="panel" style="
                            position:absolute;
                            left:50%;
                            top:12px;
                            transform:translate(-50%,-10px);
                            width:100%;
                            box-sizing:border-box;
                            opacity:0;
                            transition:opacity .16s ease,transform .16s ease;
                            pointer-events:none;
                            font:13px/1.35 system-ui,-apple-system,BlinkMacSystemFont,Segoe UI,sans-serif;
                            color:#f5f7fb;
                            background:linear-gradient(135deg,rgba(19,22,34,.82),rgba(30,22,52,.74));
                            border:1px solid rgba(142,105,255,.28);
                            box-shadow:0 18px 60px rgba(0,0,0,.34),0 0 28px rgba(126,87,255,.12) inset;
                            backdrop-filter:blur(18px);
                            border-radius:8px;
                            padding:11px 12px;
                            overflow:hidden">
                            <div style="display:flex;gap:10px;align-items:flex-start">
                                <div style="width:9px;height:9px;border-radius:50%;background:#8b5cf6;box-shadow:0 0 12px rgba(139,92,246,.9);margin-top:5px;flex:0 0 auto"></div>
                                <div style="min-width:0;flex:1">
                                    <div data-role="badge" style="font-weight:700;white-space:nowrap;overflow:hidden;text-overflow:ellipsis"></div>
                                    <div data-role="event" style="margin-top:3px;color:rgba(226,232,240,.78);overflow-wrap:anywhere"></div>
                                </div>
                                <button data-role="leave" style="display:none;border:1px solid rgba(255,255,255,.12);border-radius:6px;background:rgba(255,255,255,.08);color:#f8fafc;padding:6px 9px;font:inherit;cursor:pointer;flex:0 0 auto">Leave</button>
                            </div>
                            <div data-role="members" style="display:grid;grid-template-columns:1fr;gap:5px;margin-top:10px;max-height:86px;overflow:auto"></div>
                        </div>
                    `;
                    const panel = el.querySelector('[data-role="panel"]');
                    const show = () => {
                        panel.style.opacity = "1";
                        panel.style.transform = "translate(-50%,0)";
                        panel.style.pointerEvents = "auto";
                    };
                    const hide = () => {
                        if (el.matches(":hover")) return;
                        panel.style.opacity = "0";
                        panel.style.transform = "translate(-50%,-10px)";
                        panel.style.pointerEvents = "none";
                    };
                    el.querySelector('[data-role="hotspot"]').addEventListener("mouseenter", show);
                    panel.addEventListener("mouseenter", show);
                    el.addEventListener("mouseleave", () => setTimeout(hide, 120));
                    el.__stremioWatchPartyShow = show;
                    el.__stremioWatchPartyHide = hide;
                    el.querySelector('[data-role="leave"]').addEventListener("click", () => {
                        try {
                            window.chrome.webview.postMessage(JSON.stringify({
                                id: 1,
                                args: ["shell-watch-party-leave"]
                            }));
                        } catch (_) {}
                    });
                }
                el.querySelector('[data-role="badge"]').textContent = state.badge || "Watch Party";
                const event = el.querySelector('[data-role="event"]');
                const message = [state.eventTitle, state.eventText].filter(Boolean).join(": ");
                event.textContent = message || (state.active ? "Playback sync is active." : "");
                const leave = el.querySelector('[data-role="leave"]');
                leave.style.display = state.canLeave ? "block" : "none";
                const members = el.querySelector('[data-role="members"]');
                members.innerHTML = "";
                (state.members || []).forEach((member) => {
                    const row = document.createElement("div");
                    row.style.cssText = "display:flex;align-items:center;gap:8px;min-width:0;color:rgba(241,245,249,.9)";
                    const dot = document.createElement("div");
                    dot.style.cssText = `width:6px;height:6px;border-radius:50%;flex:0 0 auto;background:${member.isHost ? "#8b5cf6" : "#38bdf8"}`;
                    const name = document.createElement("div");
                    name.textContent = `${member.name || "Unknown"}${member.isHost ? " - Host" : ""}`;
                    name.style.cssText = "min-width:0;flex:1;white-space:nowrap;overflow:hidden;text-overflow:ellipsis";
                    row.appendChild(dot);
                    row.appendChild(name);
                    if (state.canKick && !member.isHost) {
                        const kick = document.createElement("button");
                        kick.textContent = "Remove";
                        kick.style.cssText = "border:1px solid rgba(255,255,255,.12);border-radius:6px;background:rgba(239,68,68,.16);color:#fecaca;padding:4px 7px;font:inherit;cursor:pointer;flex:0 0 auto";
                        kick.addEventListener("click", () => {
                            try {
                                window.chrome.webview.postMessage(JSON.stringify({
                                    id: 1,
                                    args: ["shell-watch-party-kick", { steamId: String(member.steamId) }]
                                }));
                            } catch (_) {}
                        });
                        row.appendChild(kick);
                    }
                    members.appendChild(row);
                });
                if (message && el.__stremioWatchPartyShow) {
                    el.__stremioWatchPartyShow();
                    clearTimeout(el.__stremioWatchPartyTimer);
                    el.__stremioWatchPartyTimer = setTimeout(() => {
                        if (el.__stremioWatchPartyHide) el.__stremioWatchPartyHide();
                    }, 4200);
                }
            })();"##
        .replace("__STREMIO_WATCH_PARTY_STATE__", &payload.to_string());

        send_webview_script(&script);
    }
    fn on_leave_watch_party(&self) {
        let role = LOBBY_ROLE.lock().map(|r| *r).unwrap_or(LobbyRole::None);
        if role == LobbyRole::Host {
            self.tray.tray.show(
                "The host keeps the watch party open. Guests can leave from their side.",
                Some("Watch Party"),
                Some(nwg::TrayNotificationFlags::INFO_ICON),
                None,
            );
            return;
        }

        if let Err(e) = crate::stremio_app::steam_sync::leave_lobby() {
            self.tray.tray.show(
                &format!("Could not leave watch party: {e}"),
                Some("Watch Party Error"),
                Some(nwg::TrayNotificationFlags::ERROR_ICON),
                None,
            );
        }
        self.update_watch_party_menu();
    }
    fn on_toggle_topmost(&self) {
        if let Some(hwnd) = self.window.handle.hwnd() {
            if let Ok(mut saved_style) = self.saved_window_style.try_borrow_mut() {
                saved_style.toggle_topmost(hwnd);
                self.tray
                    .tray_topmost
                    .set_checked((saved_style.ex_style as u32 & WS_EX_TOPMOST) == WS_EX_TOPMOST);
            }
        }
    }
    fn on_show(&self) {
        self.window.set_visible(true);
        if let (Some(hwnd), Ok(mut saved_style)) = (
            self.window.handle.hwnd(),
            self.saved_window_style.try_borrow_mut(),
        ) {
            if saved_style.is_window_minimized(hwnd) {
                self.window.restore();
            }
            saved_style.set_active(hwnd);
        }
        self.tray.tray_show_hide.set_checked(self.window.visible());
        self.transmit_window_state_change();
        self.transmit_window_visibility_change();
    }
    fn on_show_hide(&self) {
        if self.window.visible() {
            self.window.set_visible(false);
            self.tray.tray_show_hide.set_checked(self.window.visible());
            self.transmit_window_state_change();
            self.transmit_window_visibility_change();
        } else {
            self.on_show();
        }
    }
    fn on_quit(&self, data: &nwg::EventData) {
        if let nwg::EventData::OnWindowClose(data) = data {
            data.close(false);
        }
        self.save_window_settings();
        self.window.set_visible(false);
        self.tray.tray_show_hide.set_checked(self.window.visible());
        self.transmit_window_visibility_change();
    }
    fn on_exit(&self) {
        if self.pip_active.get() {
            *self.requested_pip.lock().unwrap() = Some(false);
            self.on_toggle_pip_notice();
        } else {
            self.save_pip_placement();
        }
        self.save_window_settings();
        nwg::stop_thread_dispatch();
    }
}

fn build_detail_activity(
    drp: &mut DiscordIpcClient,
    config: &Config,
    info: &VideoInfo,
    media_type: &str,
    cur_url: &str,
    app_start_time: SystemTime,
) -> Result<(), Box<dyn std::error::Error>> {
    let _ = app_start_time; // derived start time or use passed time

    let large_text = format!("{} ({})", info.name, info.year);
    let poster_url = weserv_contain(&info.poster);
    let mut assets = Assets::new()
        .large_image(&poster_url)
        .large_text(&large_text);

    if config.show_small_image {
        assets = assets.small_image(ICON_URL).small_text("Stremio");
    }

    let state_text = if media_type == "series" {
        "Viewing Series"
    } else {
        "Viewing Movie"
    };

    let start_time = app_start_time.duration_since(UNIX_EPOCH)?.as_secs() as i64;

    // ── Pre-read lobby state (must outlive `activity`) ──
    let lobby_party_id = LOBBY_PARTY_ID.lock().map(|p| p.clone()).unwrap_or_default();
    let lobby_join_secret = LOBBY_JOIN_SECRET
        .lock()
        .map(|s| s.clone())
        .unwrap_or_default();
    let lobby_member_count = LOBBY_MEMBER_COUNT.lock().map(|c| *c).unwrap_or(1);
    let lobby_max_size = LOBBY_MAX_SIZE.lock().map(|m| *m).unwrap_or(8);

    let mut activity = Activity::new()
        .activity_type(ActivityType::Watching)
        .name(&info.name)
        .details(&info.name)
        .state(state_text)
        .timestamps(Timestamps::new().start(start_time))
        .assets(assets);

    let last_segment = if cur_url.contains("/series/") {
        cur_url
            .split_once("/series/")
            .map(|(_, part)| format!("/series/{}", part))
    } else if cur_url.contains("/movie/") {
        cur_url
            .split_once("/movie/")
            .map(|(_, part)| format!("/movie/{}", part))
    } else {
        None
    }
    .unwrap_or_default();

    let trimmed_segment = last_segment
        .trim_start_matches("/series/")
        .trim_start_matches("/movie/");
    let raw_id = trimmed_segment.split('/').next().unwrap_or("");
    let content_id_cow = decode(raw_id).unwrap_or(std::borrow::Cow::Borrowed(raw_id));
    let content_id = content_id_cow.as_ref();

    // Add buttons if needed (using string references)
    let (external_url, stremio_url, button_label) = if config.show_buttons && !content_id.is_empty()
    {
        let (label, url) = if content_id.starts_with("kitsu:") {
            let id_part = content_id.trim_start_matches("kitsu:");
            ("Kitsu", format!("https://kitsu.app/anime/{}", id_part))
        } else {
            ("IMDb", format!("https://www.imdb.com/title/{}", content_id))
        };

        let stremio = if config.link_target == "web" {
            format!("https://web.stremio.com/#/detail{}", last_segment)
        } else {
            format!("stremio:///detail{}", last_segment)
        };
        (Some(url), Some(stremio), label)
    } else {
        (None, None, "")
    };

    if let (Some(external), Some(stremio)) = (&external_url, &stremio_url) {
        if lobby_party_id.is_empty() {
            activity = activity.buttons(vec![
                Button::new(button_label, external),
                Button::new("Open in Stremio", stremio),
            ]);
        }
    }

    // ── Attach lobby party + join secret ──
    if !lobby_party_id.is_empty() {
        activity = activity
            .party(
                Party::new()
                    .id(&lobby_party_id)
                    .size([lobby_member_count, lobby_max_size]),
            )
            .secrets(Secrets::new().join(&lobby_join_secret));
    }

    drp.set_activity(activity)?;
    Ok(())
}
