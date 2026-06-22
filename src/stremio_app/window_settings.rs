use serde::{Deserialize, Serialize};
use std::{env, fs, io, path::PathBuf};
use winapi::shared::windef::{HWND, POINT, RECT};
use winapi::um::winuser::{
    GetWindowPlacement, IsIconic, SW_SHOWMAXIMIZED, SW_SHOWNORMAL, WINDOWPLACEMENT,
};

const WINDOW_SETTINGS_FILE: &str = "window-state.json";
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
            .and_then(|settings| serde_json::from_str(&settings).ok())
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

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WindowSettings {
    show_cmd: u32,
    min_position: Point,
    max_position: Point,
    normal_position: Rect,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Point {
    x: i32,
    y: i32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl WindowSettings {
    pub fn load() -> Option<Self> {
        fs::read_to_string(settings_path())
            .ok()
            .and_then(|settings| serde_json::from_str(&settings).ok())
    }

    pub fn save(hwnd: HWND) -> io::Result<()> {
        let Some(settings) = Self::from_window(hwnd) else {
            return Ok(());
        };
        let path = settings_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(&settings).map_err(io::Error::other)?;
        fs::write(path, json)
    }

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

        if unsafe { GetWindowPlacement(hwnd, &mut placement) } == 0 {
            return None;
        }
        if !is_restorable_size(&placement.rcNormalPosition) {
            return None;
        }

        Some(WindowSettings {
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

fn settings_path() -> PathBuf {
    env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir)
        .join("Stremio")
        .join(WINDOW_SETTINGS_FILE)
}

fn pip_settings_path() -> PathBuf {
    env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir)
        .join("Stremio")
        .join(PIP_SETTINGS_FILE)
}

fn is_restorable_size(rect: &RECT) -> bool {
    rect.right > rect.left && rect.bottom > rect.top
}

impl From<POINT> for Point {
    fn from(point: POINT) -> Self {
        Point {
            x: point.x,
            y: point.y,
        }
    }
}

impl From<Point> for POINT {
    fn from(point: Point) -> Self {
        POINT {
            x: point.x,
            y: point.y,
        }
    }
}

impl From<RECT> for Rect {
    fn from(rect: RECT) -> Self {
        Rect {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        }
    }
}

impl From<Rect> for RECT {
    fn from(rect: Rect) -> Self {
        RECT {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::is_restorable_size;
    use winapi::shared::windef::RECT;

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
}
