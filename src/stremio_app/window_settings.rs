use serde::{Deserialize, Serialize};
use std::{env, fs, io, path::PathBuf};

const PIP_SETTINGS_FILE: &str = "pip-state.json";

#[derive(Clone, Debug, Deserialize, Serialize)]
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
        fs::read_to_string(pip_settings_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
    }

    pub fn save(placement: Self) -> io::Result<()> {
        let path = pip_settings_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(&placement).map_err(io::Error::other)?;
        fs::write(path, json)
    }
}

fn pip_settings_path() -> PathBuf {
    env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir)
        .join("Stremio")
        .join(PIP_SETTINGS_FILE)
}
