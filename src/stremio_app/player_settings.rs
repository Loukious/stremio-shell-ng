use flume::{Receiver, Sender};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

const PLAYER_SETTINGS_FILE: &str = "player-state.json";
const VOLUME_SAVE_DEBOUNCE: Duration = Duration::from_millis(300);
pub const DEFAULT_VOLUME: f64 = 100.0;
const MAX_VOLUME: f64 = 130.0;

#[derive(Debug, Deserialize, Serialize)]
struct PlayerSettings {
    #[serde(default = "default_volume")]
    volume: f64,
}

static VOLUME_SAVE_TX: Lazy<Sender<f64>> = Lazy::new(|| {
    let (tx, rx) = flume::unbounded();
    thread::Builder::new()
        .name("player-settings".to_string())
        .spawn(move || volume_save_loop(rx))
        .expect("cannot start player settings thread");
    tx
});

pub fn load_volume() -> f64 {
    load_volume_from_path(&settings_path())
}

fn load_volume_from_path(path: &Path) -> f64 {
    fs::read_to_string(path)
        .ok()
        .and_then(|settings| serde_json::from_str::<PlayerSettings>(&settings).ok())
        .map(|settings| sanitize_volume(settings.volume))
        .unwrap_or(DEFAULT_VOLUME)
}

pub fn save_volume_debounced(volume: f64) {
    if volume.is_finite() {
        VOLUME_SAVE_TX.send(sanitize_volume(volume)).ok();
    }
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

        match save_volume(volume) {
            Ok(()) => last_saved = volume,
            Err(error) => eprintln!("Cannot save player volume: {error}"),
        }
    }
}

fn save_volume(volume: f64) -> io::Result<()> {
    let path = settings_path();
    save_volume_to_path(&path, volume)
}

fn save_volume_to_path(path: &Path, volume: f64) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let settings = PlayerSettings {
        volume: sanitize_volume(volume),
    };
    let json = serde_json::to_string_pretty(&settings).map_err(io::Error::other)?;
    fs::write(path, json)
}

fn sanitize_volume(volume: f64) -> f64 {
    if volume.is_finite() {
        volume.clamp(0.0, MAX_VOLUME)
    } else {
        DEFAULT_VOLUME
    }
}

fn default_volume() -> f64 {
    DEFAULT_VOLUME
}

fn settings_path() -> PathBuf {
    env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir)
        .join("Stremio")
        .join(PLAYER_SETTINGS_FILE)
}

#[cfg(test)]
mod tests {
    use super::{
        load_volume_from_path, sanitize_volume, save_volume_to_path, DEFAULT_VOLUME, MAX_VOLUME,
    };
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn volume_is_limited_to_mpv_range() {
        assert_eq!(sanitize_volume(-1.0), 0.0);
        assert_eq!(sanitize_volume(52.5), 52.5);
        assert_eq!(sanitize_volume(500.0), MAX_VOLUME);
    }

    #[test]
    fn invalid_volume_uses_default() {
        assert_eq!(sanitize_volume(f64::NAN), DEFAULT_VOLUME);
        assert_eq!(sanitize_volume(f64::INFINITY), DEFAULT_VOLUME);
    }

    #[test]
    fn saved_volume_round_trips() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "stremio-shell-player-settings-{}-{unique}.json",
            std::process::id()
        ));

        save_volume_to_path(&path, 47.25).unwrap();
        assert_eq!(load_volume_from_path(&path), 47.25);

        fs::remove_file(path).ok();
    }
}
