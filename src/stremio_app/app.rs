use discord_rich_presence::{
    activity::{Activity, ActivityType, Assets, Button, Timestamps},
    DiscordIpc, DiscordIpcClient,
};
use flume::{Receiver, Sender};
use ini::Ini;
use native_windows_derive::NwgUi;
use native_windows_gui as nwg;
use once_cell::sync::Lazy;
use rand::Rng;
use reqwest::blocking::Client;
use serde_json::{self, Value};
use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::{
    cell::RefCell,
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
use winapi::um::{winbase::CREATE_BREAKAWAY_FROM_JOB, winuser::WS_EX_TOPMOST};
struct SafeHwnd(*mut c_void);
unsafe impl Send for SafeHwnd {}

pub static VIDEO_TITLE: Lazy<Mutex<String>> = Lazy::new(|| Mutex::new("".to_string()));

pub static COVER_URL: Lazy<Mutex<String>> = Lazy::new(|| Mutex::new("".to_string()));

pub static ALBUM: Lazy<Mutex<String>> = Lazy::new(|| Mutex::new("".to_string()));

pub const ICON_URL: &str =
    "https://raw.githubusercontent.com/Stremio/stremio-web/refs/heads/development/assets/images/icon.png";

use crate::stremio_app::{
    constants::{
        web_endpoint_with_streaming_server, APP_NAME, UPDATE_ENDPOINT, UPDATE_INTERVAL,
        WEB_ENDPOINT, WINDOW_MIN_HEIGHT, WINDOW_MIN_WIDTH,
    },
    ipc::{RPCRequest, RPCResponse},
    splash::SplashImage,
    stremio_player::{
        player::{CURRENT_TIME, IS_PAUSED, TOTAL_DURATION},
        Player,
    },
    stremio_wevbiew::{wevbiew::CURRENT_URL, WebView},
    systray::SystemTray,
    updater,
    window_helper::WindowStyle,
    window_settings::WindowSettings,
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
        (tray_show_hide, OnMenuItemSelected): [Self::on_show_hide],
        (tray_topmost, OnMenuItemSelected): [Self::on_toggle_topmost],
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
    #[nwg_events(OnNotice: [nwg::stop_thread_dispatch()] )]
    pub quit_notice: nwg::Notice,
    #[nwg_control]
    #[nwg_events(OnNotice: [Self::on_hide_splash_notice] )]
    pub hide_splash_notice: nwg::Notice,
    #[nwg_control]
    #[nwg_events(OnNotice: [Self::on_focus_notice] )]
    pub focus_notice: nwg::Notice,
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
            .set("show_small_image", "true");

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

    // Return the parsed configuration
    Config {
        show_buttons,
        link_target,
        disable_in_menu,
        disable_when_paused,
        refresh_interval,
        show_small_image,
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

pub fn spawn_discordrpc_loop(app_start_time: SystemTime) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let config = load_or_create_config();
        let retry_count = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));

        loop {
            let current_retry = retry_count.clone();
            let result = catch_unwind(AssertUnwindSafe(|| {
                let mut drp = DiscordIpcClient::new("997798118185771059");

                loop {  // Connection maintenance loop
                    // Attempt connection
                    match drp.connect() {
                        Ok(_) => {
                            current_retry.store(0, std::sync::atomic::Ordering::SeqCst);
                            println!("✅ Connected to Discord IPC");
                        },
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

                    loop {  // Activity update loop
                        let sleep_time = Duration::from_secs(config.refresh_interval);
                        thread::sleep(sleep_time);

                        // Safely get current state with error handling
                        let (cur_url, cur_time, is_paused, total_duration) = match (
                            CURRENT_URL.lock(),
                            CURRENT_TIME.lock(),
                            IS_PAUSED.lock(),
                            TOTAL_DURATION.lock(),
                        ) {
                            (Ok(url), Ok(time), Ok(paused), Ok(duration)) => (
                                url.clone(),
                                *time,
                                *paused,
                                *duration,
                            ),
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
                                    let video_id = match decode(cur_url.split('/').last().unwrap_or("")) {
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
                                            drp.clear_activity().map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
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
                                drp.clear_activity().map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
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
                    }  // End activity update loop

                    let _ = drp.close();
                }  // End connection loop
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
    let now_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_secs() as i64;

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

    let (activity_name, details, state_text) = if media_type == "series" {
        (
            info.name.clone(),
            info.epname.clone(),
            format!("S{}E{}", season, episode),
        )
    } else {
        (info.name.clone(), info.name.clone(), info.year.clone())
    };

    let large_text = format!("{} ({})", info.name, info.year);
    let poster_url = weserv_contain(&info.poster);
    let mut assets = Assets::new()
        .large_image(&poster_url)
        .large_text(&large_text);

    if config.show_small_image {
        let (small_image, small_text) = if is_paused {
            (
                "https://i.imgur.com/eCUJpm9.png", // Paused icon
                "Paused"
            )
        } else {
            (
                ICON_URL,
                "Playing"
            )
        };
    
        assets = assets
            .small_image(small_image)
            .small_text(small_text);
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

    // Add buttons if needed
    if let (Some(external), Some(stremio)) = (&external_url, &stremio_url) {
        activity = activity.buttons(vec![
            Button::new(button_label, external),
            Button::new("Open in Stremio", stremio),
        ]);
    }

    drp.set_activity(activity)?;
    Ok(())
}



fn build_menu_activity(
    drp: &mut DiscordIpcClient,
    cur_url: &str,
    app_start_time: SystemTime,
) -> Result<(), Box<dyn std::error::Error>> {
    let start_time = app_start_time
        .duration_since(UNIX_EPOCH)?
        .as_secs() as i64;
    
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
        if let (Ok(web_channel), Ok(style)) = (
            self.webview.channel.try_borrow(),
            self.saved_window_style.try_borrow(),
        ) {
            let (web_tx, _) = web_channel
                .as_ref()
                .expect("Cannont obtain communication channel for the Web UI");
            let web_tx_app = web_tx.clone();
            web_tx_app
                .send(RPCResponse::visibility_change(
                    self.window.visible(),
                    style.full_screen as u32,
                    style.full_screen,
                ))
                .ok();
        } else {
            eprintln!("Cannot obtain communication channel or window style");
        }
    }
    fn transmit_window_state_change(&self) {
        if let (Some(hwnd), Ok(web_channel), Ok(style)) = (
            self.window.handle.hwnd(),
            self.webview.channel.try_borrow(),
            self.saved_window_style.try_borrow(),
        ) {
            let state = style.clone().get_window_state(hwnd);
            drop(style);
            let (web_tx, _) = web_channel
                .as_ref()
                .expect("Cannont obtain communication channel for the Web UI");
            let web_tx_app = web_tx.clone();
            web_tx_app.send(RPCResponse::state_change(state)).ok();
        } else {
            eprintln!("Cannot obtain window handle or communication channel");
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
        }

        let app_start_time = SystemTime::now();
        spawn_discordrpc_loop(app_start_time);

        self.window.set_visible(!self.start_hidden);
        self.tray.tray_show_hide.set_checked(!self.start_hidden);
        if self.no_splash {
            self.splash_screen.hide();
        }

        let player_channel = self.player.channel.borrow();
        let (player_tx, player_rx) = player_channel
            .as_ref()
            .expect("Cannont obtain communication channel for the Player");
        let player_tx = player_tx.clone();
        let player_rx = player_rx.clone();

        let web_channel = self.webview.channel.borrow();
        let (web_tx, web_rx) = web_channel
            .as_ref()
            .expect("Cannont obtain communication channel for the Web UI");
        let web_tx_player = web_tx.clone();
        let web_tx_web = web_tx.clone();
        let web_tx_arg = web_tx.clone();
        let web_tx_upd = web_tx.clone();
        let web_rx = web_rx.clone();

        let (updater_tx, updater_rx) = flume::unbounded::<String>();
        let updater_tx_web = updater_tx.clone();

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

                let updater_endpoint = if let Some(ref endpoint) = autoupdater_endpoint {
                    endpoint.clone()
                } else {
                    let mut rng = rand::thread_rng();
                    let index = rng.gen_range(0..UPDATE_ENDPOINT.len());
                    let mut url = Url::parse(UPDATE_ENDPOINT[index]).unwrap();
                    url.query_pairs_mut().append_pair("arch", env!("ARCH"));
                    if release_candidate {
                        url.query_pairs_mut().append_pair("rc", "true");
                    }
                    url
                };

                let updater =
                    updater::Updater::new(current_version, &updater_endpoint, force_update);
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
                        // ['open-media', url]
                        web_tx_arg.send(RPCResponse::open_media(s.to_string())).ok();
                        println!("{s}");
                    }
                }
            });
        }

        // Read message from player
        thread::spawn(move || loop {
            player_rx
                .iter()
                .map(|msg| web_tx_player.send(msg))
                .for_each(drop);
        }); // thread

        let toggle_fullscreen_sender = self.toggle_fullscreen_notice.sender();
        let quit_sender = self.quit_notice.sender();
        let hide_splash_sender = self.hide_splash_notice.sender();
        let focus_sender = self.focus_notice.sender();
        let autoupdater_setup_mutex = self.autoupdater_setup_file.clone();

        let discord_rpc = DiscordRpc::new(web_tx.clone());
        let requested_fullscreen = self.requested_fullscreen.clone();

        thread::spawn(move || loop {
            if let Some(msg) = web_rx
                .recv()
                .ok()
                .and_then(|s| serde_json::from_str::<RPCRequest>(&s).ok())
            {
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
                    Some("quit") => quit_sender.notice(),
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
                            web_tx_web.send(RPCResponse::open_media(command_ref)).ok();
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
                        let resp_json = serde_json::to_string(
                            &msg.args.expect("Cannot have method without args"),
                        )
                        .expect("Cannot build response");
                        player_tx.send(resp_json).ok();
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
        assets = assets
            .small_image(ICON_URL)
            .small_text("Stremio");
    }

    let state_text = if media_type == "series" {
        "Viewing Series"
    } else {
        "Viewing Movie"
    };

    let start_time = app_start_time
        .duration_since(UNIX_EPOCH)?
        .as_secs() as i64;

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
    let (external_url, stremio_url, button_label) = if config.show_buttons && !content_id.is_empty() {
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
        activity = activity.buttons(vec![
            Button::new(button_label, external),
            Button::new("Open in Stremio", stremio),
        ]);
    }

    drp.set_activity(activity)?;
    Ok(())
}
