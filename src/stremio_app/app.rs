use discord_rich_presence::{
    activity::{Activity, ActivityType, Assets, Button, Timestamps},
    DiscordIpc, DiscordIpcClient,
};
use flume::{Receiver, Sender};
use ini::Ini;
use native_windows_derive::NwgUi;
use native_windows_gui as nwg;
use rand::Rng;
use reqwest::blocking::Client;
use serde_json::{self, Value};
use souvlaki::{MediaControlEvent, MediaControls, MediaPlayback, PlatformConfig};
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

use crate::stremio_app::{
    constants::{APP_NAME, UPDATE_ENDPOINT, UPDATE_INTERVAL, WINDOW_MIN_HEIGHT, WINDOW_MIN_WIDTH},
    ipc::{RPCRequest, RPCResponse},
    splash::SplashImage,
    stremio_player::{
        player::{CURRENT_TIME, TOTAL_DURATION},
        Player,
    },
    stremio_wevbiew::{wevbiew::CURRENT_URL, WebView},
    systray::SystemTray,
    updater,
    window_helper::WindowStyle,
    PipeServer,
};

use super::stremio_server::StremioServer;

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
}

#[derive(Default, NwgUi)]
pub struct MainWindow {
    pub command: String,
    pub commands_path: Option<String>,
    pub webui_url: String,
    pub dev_tools: bool,
    pub start_hidden: bool,
    pub autoupdater_endpoint: Option<Url>,
    pub force_update: bool,
    pub release_candidate: bool,
    pub autoupdater_setup_file: Arc<Mutex<Option<PathBuf>>>,
    pub saved_window_style: RefCell<WindowStyle>,
    #[nwg_resource]
    pub embed: nwg::EmbedResource,
    #[nwg_resource(source_embed: Some(&data.embed), source_embed_str: Some("MAINICON"))]
    pub window_icon: nwg::Icon,
    #[nwg_control(icon: Some(&data.window_icon), title: APP_NAME, flags: "MAIN_WINDOW")]
    #[nwg_events( OnWindowClose: [Self::on_quit(SELF, EVT_DATA)], OnInit: [Self::on_init], OnPaint: [Self::on_paint], OnMinMaxInfo: [Self::on_min_max(SELF, EVT_DATA)], OnWindowMinimize: [Self::transmit_window_state_change], OnWindowMaximize: [Self::transmit_window_state_change] )]
    pub window: nwg::Window,
    #[nwg_partial(parent: window)]
    #[nwg_events((tray, MousePressLeftUp): [Self::on_show], (tray_exit, OnMenuItemSelected): [nwg::stop_thread_dispatch()], (tray_show_hide, OnMenuItemSelected): [Self::on_show_hide], (tray_topmost, OnMenuItemSelected): [Self::on_toggle_topmost]) ]
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
            .set("refresh_interval", "5");

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

    // Return the parsed configuration
    Config {
        show_buttons,
        link_target,
        disable_in_menu,
        disable_when_paused,
        refresh_interval,
    }
}

pub fn getvidinfo(type_: &str, id: &str, season: &str, episode: &str) -> Option<VideoInfo> {
    let url = format!("https://v3-cinemeta.strem.io/meta/{}/{}.json", type_, id);

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
                        let expected_id = format!("{}:{}:{}", id, season, episode);
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

fn parse_video_id(video_id: &str) -> (&str, &str, &str, &str) {
    let parts: Vec<&str> = video_id.split(':').collect();
    match parts.len() {
        1 => ("movie", parts[0], "", ""),              // Movie case
        3 => ("series", parts[0], parts[1], parts[2]), // Series case
        _ => ("unknown", "", "", ""),
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
                MediaControlEvent::Play => {
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
                _ => {}
            }
        })
        .expect("Cannot attach media key callback");

    // Possibly set initial metadata
    controls
        .set_playback(MediaPlayback::Paused { progress: None })
        .ok();

    // Keep it alive
    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

pub fn spawn_discordrpc_loop(app_start_time: SystemTime) -> thread::JoinHandle<()> {
    let mut drp =
        DiscordIpcClient::new("997798118185771059").expect("Failed to create Discord IPC client");

    drp.connect().expect("Failed to connect to Discord IPC");

    // Track previous state
    let mut last_url = String::new();
    let mut last_time = 0.0;
    let mut video_info: Option<VideoInfo> = None;
    let mut type_ = String::new();
    let mut season = String::new();
    let mut episode = String::new();

    let config = load_or_create_config();

    thread::spawn(move || loop {
        {
            thread::sleep(Duration::from_secs(config.refresh_interval));
            let cur_url = CURRENT_URL.lock().unwrap().clone();
            let cur_time = *CURRENT_TIME.lock().unwrap();

            if cur_url.starts_with("https://web.stremio.com/#/player") {
                // If the URL has changed, fetch video info
                if cur_url != last_url {
                    let video_id = decode(cur_url.split("/").last().unwrap()).expect("UTF-8");
                    let (parsed_type, parsed_id, parsed_season, parsed_episode) =
                        parse_video_id(&video_id);

                    type_ = parsed_type.to_owned();
                    season = parsed_season.to_owned();
                    episode = parsed_episode.to_owned();

                    video_info = getvidinfo(&type_, &parsed_id, &season, &episode);

                    // Update last URL
                    last_url = cur_url.clone();
                }

                // If video info is available, update the presence
                if let Some(ref info) = video_info {
                    let tot_dur = *TOTAL_DURATION.lock().unwrap();

                    let now_unix = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs() as i64;

                    let start_timestamp = now_unix - cur_time as i64;
                    let end_timestamp = start_timestamp + tot_dur as i64;

                    let large_image_text = format!("{} ({})", info.name, info.year);
                    let large_image = &info.poster;
                    let details = &info.name;
                    let state_text;
                    let small_image_text;
                    let small_image;
                    let mut timestamps =
                        Timestamps::new().start(start_timestamp).end(end_timestamp);

                    // Handle paused state
                    if cur_time == last_time {
                        if config.disable_when_paused {
                            drp.clear_activity().ok();
                            continue;
                        }
                        timestamps = Timestamps::new().start(
                            app_start_time.duration_since(UNIX_EPOCH).unwrap().as_secs() as i64,
                        );
                        small_image_text = Some("Paused".to_string());
                        small_image = Some("https://i.imgur.com/eCUJpm9.png".to_string());
                    } else {
                        small_image_text = Some("Playing".to_string());
                        if type_ == "series" {
                            small_image = Some(info.thumbnail.to_string());
                        } else {
                            small_image = Some("https://raw.githubusercontent.com/Stremio/stremio-web/refs/heads/development/images/icon.png".to_string());
                        }
                    }

                    if type_ == "series" {
                        state_text = format!("{} (S{}-E{})", info.epname, season, episode);
                    } else {
                        state_text = info.year.clone();
                    }

                    let mut assets = Assets::new()
                        .large_image(&large_image)
                        .large_text(&large_image_text);

                    if let Some(small_text) = &small_image_text {
                        assets = assets.small_text(small_text);
                    }
                    if let Some(small_img) = &small_image {
                        assets = assets.small_image(small_img);
                    }

                    // Determine the last segment for IMDb/Stremio links
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
                    let imdb_id = trimmed_segment.split('/').next().unwrap_or("");
                    let imdb_url_str = format!("https://www.imdb.com/title/{}", imdb_id);
                    let stremio_url_str = if config.link_target == "web" {
                        format!("https://web.stremio.com/#/detail{}", last_segment)
                    } else {
                        format!("stremio:///detail{}", last_segment)
                    };

                    let mut activity = Activity::new()
                        .activity_type(ActivityType::Watching)
                        .state(&state_text)
                        .details(details)
                        .timestamps(timestamps)
                        .assets(assets);

                    // Conditionally add buttons (if configured to show)
                    if config.show_buttons {
                        activity = activity.buttons(vec![
                            Button::new("View on IMDb", &imdb_url_str),
                            Button::new("Open in Stremio", &stremio_url_str),
                        ]);
                    }

                    if let Err(e) = drp.set_activity(activity) {
                        eprintln!("Failed to set Discord activity: {e}");
                    }
                }

                last_time = cur_time;
            } else {
                if config.disable_in_menu {
                    drp.clear_activity().ok();
                    continue;
                }
                let activity =
                    Activity::new()
                        .activity_type(ActivityType::Watching)
                        .state("In Menu")
                        .details("Browsing catalog")
                        .timestamps(Timestamps::new().start(
                            app_start_time.duration_since(UNIX_EPOCH).unwrap().as_secs() as i64,
                        ))
                        .assets(
                            Assets::new()
                                .large_image("https://raw.githubusercontent.com/Stremio/stremio-web/refs/heads/development/images/icon.png")
                                .large_text("Stremio")
                        );

                if let Err(e) = drp.set_activity(activity) {
                    eprintln!("Failed to set Discord activity: {e}");
                }

                // Clear the last URL and video info
                last_url.clear();
                video_info = None;
            }
        }
    })
}

impl MainWindow {
    fn transmit_window_full_screen_change(&self, full_screen: bool, prevent_close: bool) {
        let web_channel = self.webview.channel.borrow();
        let (web_tx, _) = web_channel
            .as_ref()
            .expect("Cannont obtain communication channel for the Web UI");
        let web_tx_app = web_tx.clone();
        web_tx_app
            .send(RPCResponse::visibility_change(
                self.window.visible(),
                prevent_close as u32,
                full_screen,
            ))
            .ok();
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
        self.webview.endpoint.set(self.webui_url.clone()).ok();
        self.webview.dev_tools.set(self.dev_tools).ok();
        if let Some(hwnd) = self.window.handle.hwnd() {
            let player_channel = self.player.channel.borrow().clone();
            let safe_hwnd = SafeHwnd(hwnd as *mut c_void);

            std::thread::spawn(move || {
                run_souvlaki_media_keys(safe_hwnd.0, player_channel);
            });

            if let Ok(mut saved_style) = self.saved_window_style.try_borrow_mut() {
                saved_style.center_window(hwnd, WINDOW_MIN_WIDTH, WINDOW_MIN_HEIGHT);
            }
        }

        let app_start_time = SystemTime::now();
        spawn_discordrpc_loop(app_start_time);

        self.window.set_visible(!self.start_hidden);
        self.tray.tray_show_hide.set_checked(!self.start_hidden);

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
        thread::spawn(move || loop {
            let current_version = env!("CARGO_PKG_VERSION")
                .parse()
                .expect("Should always be valid");
            let updater_endpoint = if let Some(ref endpoint) = autoupdater_endpoint {
                endpoint.clone()
            } else {
                let mut rng = rand::thread_rng();
                let index = rng.gen_range(0..UPDATE_ENDPOINT.len());
                let mut url = Url::parse(UPDATE_ENDPOINT[index]).unwrap();
                if release_candidate {
                    url.query_pairs_mut().append_pair("rc", "true");
                }
                url
            };
            let updater = updater::Updater::new(current_version, &updater_endpoint, force_update);

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
                        println!("{}", s);
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
                    Some("win-set-visibility") => toggle_fullscreen_sender.notice(),
                    Some("quit") => quit_sender.notice(),
                    Some("app-ready") => {
                        hide_splash_sender.notice();
                        web_tx_web
                            .send(RPCResponse::visibility_change(true, 1, false))
                            .ok();
                        let command_ref = command_clone.clone();
                        if !command_ref.is_empty() {
                            web_tx_web.send(RPCResponse::open_media(command_ref)).ok();
                        }
                    }
                    Some("app-error") => {
                        hide_splash_sender.notice();
                        if let Some(arg) = msg.get_params() {
                            // TODO: Make this modal dialog
                            eprintln!("Web App Error: {}", arg);
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
                                println!("Running the setup at {:?}", file_path);

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
    fn on_toggle_fullscreen_notice(&self) {
        if let Some(hwnd) = self.window.handle.hwnd() {
            if let Ok(mut saved_style) = self.saved_window_style.try_borrow_mut() {
                saved_style.toggle_full_screen(hwnd);
                self.tray.tray_topmost.set_enabled(!saved_style.full_screen);
                self.tray
                    .tray_topmost
                    .set_checked((saved_style.ex_style as u32 & WS_EX_TOPMOST) == WS_EX_TOPMOST);
                self.transmit_window_full_screen_change(saved_style.full_screen, true);
            }
        }
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
    }
    fn on_show_hide(&self) {
        if self.window.visible() {
            self.window.set_visible(false);
            self.tray.tray_show_hide.set_checked(self.window.visible());
            self.transmit_window_state_change();
        } else {
            self.on_show();
        }
    }
    fn on_quit(&self, data: &nwg::EventData) {
        if let nwg::EventData::OnWindowClose(data) = data {
            data.close(false);
        }
        self.window.set_visible(false);
        self.tray.tray_show_hide.set_checked(self.window.visible());
        self.transmit_window_full_screen_change(false, false);
    }
}
