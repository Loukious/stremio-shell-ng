use flume::{Receiver, Sender};
use once_cell::sync::Lazy;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    env, fs, io,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
    thread,
    time::Duration,
};
use winapi::shared::windef::{HWND, POINT, RECT};
use winapi::um::winuser::{
    GetWindowPlacement, IsIconic, SW_SHOWMAXIMIZED, SW_SHOWNORMAL, WINDOWPLACEMENT,
};

const SHELL_STATE_FILE: &str = "shell-state.json";
const LEGACY_WINDOW_STATE_FILE: &str = "window-state.json";
const LEGACY_PIP_STATE_FILE: &str = "pip-state.json";
const LEGACY_PLAYER_STATE_FILE: &str = "player-state.json";
const SHELL_STATE_VERSION: u32 = 1;
const VOLUME_SAVE_DEBOUNCE: Duration = Duration::from_millis(300);
pub const DEFAULT_VOLUME: f64 = 100.0;
const MAX_VOLUME: f64 = 130.0;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PipPlacement {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    #[serde(default)]
    pub transparent: bool,
}

impl PipPlacement {
    pub fn load() -> Option<Self> {
        shell_state_lock().pip.clone()
    }

    pub fn save(placement: Self) -> io::Result<()> {
        update_shell_state(|state| state.pip = Some(placement))
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct WindowSettings {
    show_cmd: u32,
    min_position: Point,
    max_position: Point,
    normal_position: Rect,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
struct Point {
    x: i32,
    y: i32,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl WindowSettings {
    pub fn to_window_placement(&self) -> WINDOWPLACEMENT {
        let mut placement = WINDOWPLACEMENT {
            length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
            flags: 0,
            showCmd: self.show_cmd,
            ptMinPosition: self.min_position.clone().into(),
            ptMaxPosition: self.max_position.clone().into(),
            rcNormalPosition: self.normal_position.clone().into(),
        };
        if !is_restorable_size(&placement.rcNormalPosition) {
            placement.showCmd = SW_SHOWNORMAL as u32;
        }
        placement
    }

    fn from_window(hwnd: HWND) -> Option<Self> {
        if unsafe { IsIconic(hwnd) } != 0 {
            return None;
        }

        let mut placement = WINDOWPLACEMENT {
            length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
            flags: 0,
            showCmd: 0,
            ptMinPosition: POINT { x: 0, y: 0 },
            ptMaxPosition: POINT { x: 0, y: 0 },
            rcNormalPosition: RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
        };

        if unsafe { GetWindowPlacement(hwnd, &mut placement) } == 0
            || !is_restorable_size(&placement.rcNormalPosition)
        {
            return None;
        }

        Some(Self {
            show_cmd: if placement.showCmd == SW_SHOWMAXIMIZED as u32 {
                SW_SHOWMAXIMIZED as u32
            } else {
                SW_SHOWNORMAL as u32
            },
            min_position: placement.ptMinPosition.into(),
            max_position: placement.ptMaxPosition.into(),
            normal_position: placement.rcNormalPosition.into(),
        })
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
struct ShellState {
    #[serde(default = "current_state_version")]
    version: u32,
    #[serde(default)]
    window: Option<WindowSettings>,
    #[serde(default)]
    fullscreen: bool,
    #[serde(default)]
    pip: Option<PipPlacement>,
    #[serde(default = "default_volume")]
    volume: f64,
}

impl Default for ShellState {
    fn default() -> Self {
        Self {
            version: SHELL_STATE_VERSION,
            window: None,
            fullscreen: false,
            pip: None,
            volume: DEFAULT_VOLUME,
        }
    }
}

#[derive(Debug, Deserialize)]
struct LegacyPlayerState {
    #[serde(default = "default_volume")]
    volume: f64,
}

#[derive(Clone, Debug)]
struct StatePaths {
    current: PathBuf,
    legacy_window: PathBuf,
    legacy_pip: PathBuf,
    legacy_player: PathBuf,
}

impl StatePaths {
    fn new(directory: PathBuf) -> Self {
        Self {
            current: directory.join(SHELL_STATE_FILE),
            legacy_window: directory.join(LEGACY_WINDOW_STATE_FILE),
            legacy_pip: directory.join(LEGACY_PIP_STATE_FILE),
            legacy_player: directory.join(LEGACY_PLAYER_STATE_FILE),
        }
    }
}

static SHELL_STATE: Lazy<Mutex<ShellState>> =
    Lazy::new(|| Mutex::new(load_or_migrate_state(&state_paths())));

static VOLUME_SAVE_TX: Lazy<Sender<f64>> = Lazy::new(|| {
    let (tx, rx) = flume::unbounded();
    thread::Builder::new()
        .name("shell-state".to_string())
        .spawn(move || volume_save_loop(rx))
        .expect("cannot start shell state thread");
    tx
});

pub fn load_window_state() -> (Option<WindowSettings>, bool) {
    let state = shell_state_lock();
    (state.window.clone(), state.fullscreen)
}

pub fn save_window_state(hwnd: HWND, fullscreen: bool) -> io::Result<()> {
    let placement = (!fullscreen)
        .then(|| WindowSettings::from_window(hwnd))
        .flatten();

    update_shell_state(|state| {
        state.fullscreen = fullscreen;
        if let Some(placement) = placement {
            state.window = Some(placement);
        }
    })
}

pub fn load_volume() -> f64 {
    sanitize_volume(shell_state_lock().volume)
}

pub fn save_volume_debounced(volume: f64) {
    if volume.is_finite() {
        VOLUME_SAVE_TX.send(sanitize_volume(volume)).ok();
    }
}

pub fn save_volume_now(volume: f64) -> io::Result<()> {
    let volume = sanitize_volume(volume);
    update_shell_state(|state| state.volume = volume)
}

fn volume_save_loop(rx: Receiver<f64>) {
    let mut last_saved = load_volume();

    while let Ok(mut volume) = rx.recv() {
        while let Ok(next_volume) = rx.recv_timeout(VOLUME_SAVE_DEBOUNCE) {
            volume = next_volume;
        }

        if volume == last_saved {
            continue;
        }

        match save_volume_now(volume) {
            Ok(()) => last_saved = volume,
            Err(error) => eprintln!("Cannot save player volume: {error}"),
        }
    }
}

fn update_shell_state(update: impl FnOnce(&mut ShellState)) -> io::Result<()> {
    let mut state = shell_state_lock();
    update(&mut state);
    state.version = SHELL_STATE_VERSION;
    state.volume = sanitize_volume(state.volume);
    save_state_to_path(&state_paths().current, &state)
}

fn shell_state_lock() -> MutexGuard<'static, ShellState> {
    SHELL_STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn load_or_migrate_state(paths: &StatePaths) -> ShellState {
    if let Some(mut state) = read_json::<ShellState>(&paths.current) {
        state.version = SHELL_STATE_VERSION;
        state.volume = sanitize_volume(state.volume);
        return state;
    }

    let legacy_volume = read_json::<LegacyPlayerState>(&paths.legacy_player)
        .map(|player| sanitize_volume(player.volume))
        .unwrap_or(DEFAULT_VOLUME);
    let state = ShellState {
        window: read_json(&paths.legacy_window),
        pip: read_json(&paths.legacy_pip),
        volume: legacy_volume,
        ..ShellState::default()
    };

    let has_legacy_state = paths.legacy_window.is_file()
        || paths.legacy_pip.is_file()
        || paths.legacy_player.is_file();
    if has_legacy_state {
        match save_state_to_path(&paths.current, &state) {
            Ok(()) => {
                remove_if_file(&paths.legacy_window);
                remove_if_file(&paths.legacy_pip);
                remove_if_file(&paths.legacy_player);
            }
            Err(error) => eprintln!("Cannot migrate shell state: {error}"),
        }
    }

    state
}

fn save_state_to_path(path: &Path, state: &ShellState) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(state).map_err(io::Error::other)?;
    fs::write(path, json)
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    fs::read_to_string(path)
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
}

fn remove_if_file(path: &Path) {
    if path.is_file() {
        fs::remove_file(path).ok();
    }
}

fn sanitize_volume(volume: f64) -> f64 {
    if volume.is_finite() {
        volume.clamp(0.0, MAX_VOLUME)
    } else {
        DEFAULT_VOLUME
    }
}

fn current_state_version() -> u32 {
    SHELL_STATE_VERSION
}

fn default_volume() -> f64 {
    DEFAULT_VOLUME
}

fn state_paths() -> StatePaths {
    let directory = env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir)
        .join("Stremio");
    StatePaths::new(directory)
}

fn is_restorable_size(rect: &RECT) -> bool {
    rect.right > rect.left && rect.bottom > rect.top
}

impl From<POINT> for Point {
    fn from(point: POINT) -> Self {
        Self {
            x: point.x,
            y: point.y,
        }
    }
}

impl From<Point> for POINT {
    fn from(point: Point) -> Self {
        Self {
            x: point.x,
            y: point.y,
        }
    }
}

impl From<RECT> for Rect {
    fn from(rect: RECT) -> Self {
        Self {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        }
    }
}

impl From<Rect> for RECT {
    fn from(rect: Rect) -> Self {
        Self {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_paths(name: &str) -> StatePaths {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        StatePaths::new(std::env::temp_dir().join(format!(
            "stremio-shell-state-{name}-{}-{unique}",
            std::process::id()
        )))
    }

    fn sample_window() -> WindowSettings {
        WindowSettings {
            show_cmd: SW_SHOWNORMAL as u32,
            min_position: Point { x: 0, y: 0 },
            max_position: Point { x: 0, y: 0 },
            normal_position: Rect {
                left: 10,
                top: 20,
                right: 1010,
                bottom: 620,
            },
        }
    }

    #[test]
    fn rejects_empty_window_rect() {
        assert!(!is_restorable_size(&RECT {
            left: 10,
            top: 10,
            right: 10,
            bottom: 20,
        }));
    }

    #[test]
    fn accepts_non_empty_window_rect() {
        assert!(is_restorable_size(&RECT {
            left: 10,
            top: 10,
            right: 20,
            bottom: 20,
        }));
    }

    #[test]
    fn volume_is_limited_to_mpv_range() {
        assert_eq!(sanitize_volume(-1.0), 0.0);
        assert_eq!(sanitize_volume(52.5), 52.5);
        assert_eq!(sanitize_volume(500.0), MAX_VOLUME);
        assert_eq!(sanitize_volume(f64::NAN), DEFAULT_VOLUME);
    }

    #[test]
    fn shell_state_round_trips() {
        let paths = test_paths("round-trip");
        let state = ShellState {
            window: Some(sample_window()),
            fullscreen: true,
            pip: Some(PipPlacement {
                x: 50,
                y: 60,
                width: 640,
                height: 360,
                transparent: true,
            }),
            volume: 47.25,
            ..ShellState::default()
        };

        save_state_to_path(&paths.current, &state).unwrap();
        assert_eq!(read_json::<ShellState>(&paths.current), Some(state));

        fs::remove_dir_all(paths.current.parent().unwrap()).ok();
    }

    #[test]
    fn migrates_legacy_state_into_one_file() {
        let paths = test_paths("migration");
        let window = sample_window();
        let pip = PipPlacement {
            x: 100,
            y: 200,
            width: 480,
            height: 270,
            transparent: false,
        };
        fs::create_dir_all(paths.current.parent().unwrap()).unwrap();
        fs::write(
            &paths.legacy_window,
            serde_json::to_string(&window).unwrap(),
        )
        .unwrap();
        fs::write(&paths.legacy_pip, serde_json::to_string(&pip).unwrap()).unwrap();
        fs::write(&paths.legacy_player, r#"{"volume":63.5}"#).unwrap();

        let migrated = load_or_migrate_state(&paths);

        assert_eq!(migrated.window, Some(window));
        assert_eq!(migrated.pip, Some(pip));
        assert_eq!(migrated.volume, 63.5);
        assert!(paths.current.is_file());
        assert!(!paths.legacy_window.exists());
        assert!(!paths.legacy_pip.exists());
        assert!(!paths.legacy_player.exists());

        fs::remove_dir_all(paths.current.parent().unwrap()).ok();
    }
}
