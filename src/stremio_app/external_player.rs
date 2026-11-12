use std::{
    env, fs, os::windows::ffi::OsStrExt, os::windows::process::CommandExt, path::PathBuf,
    process::Command,
};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use native_windows_gui as nwg;
use url::Url;
use winapi::um::{
    shellapi::ShellExecuteW, winbase::CREATE_BREAKAWAY_FROM_JOB, winuser::SW_SHOWNORMAL,
};
use winreg::{
    enums::{HKEY_CLASSES_ROOT, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE},
    RegKey,
};

use crate::stremio_app::constants::APP_NAME;

const M3U_DATA_URI_PREFIX: &str = "data:application/octet-stream;charset=utf-8;base64,";
const APP_PATHS: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths";

#[derive(Debug, PartialEq)]
enum Request {
    Vlc(Url),
    Mpv(Url),
    PotPlayer(Url),
    Playlist(Url),
}

pub fn play(input: &str) -> Result<(), &'static str> {
    match parse(input)? {
        Request::Vlc(url) => {
            spawn_player("VLC", &["vlc.exe"], &[r"VideoLAN\VLC\vlc.exe"], &url, true)
        }
        Request::Mpv(url) => spawn_player(
            "MPV",
            &["mpv.exe"],
            &[r"mpv\mpv.exe", r"Programs\mpv\mpv.exe"],
            &url,
            true,
        ),
        Request::PotPlayer(url) => spawn_player(
            "PotPlayer",
            &["PotPlayerMini64.exe", "PotPlayerMini.exe"],
            &[
                r"DAUM\PotPlayer\PotPlayerMini64.exe",
                r"DAUM\PotPlayer\PotPlayerMini.exe",
            ],
            &url,
            false,
        ),
        Request::Playlist(url) => open_playlist(&url),
    }
}

fn parse(input: &str) -> Result<Request, &'static str> {
    if let Some(encoded) = input.strip_prefix(M3U_DATA_URI_PREFIX) {
        return parse_playlist(encoded).map(Request::Playlist);
    }
    let (scheme, rest) = input
        .split_once("://")
        .ok_or("unsupported external player")?;
    match scheme.to_ascii_lowercase().as_str() {
        "vlc" => stream_url(rest).map(Request::Vlc),
        "mpv" => stream_url(rest).map(Request::Mpv),
        "potplayer" => stream_url(rest).map(Request::PotPlayer),
        _ => Err("unsupported external player"),
    }
}

fn parse_playlist(encoded: &str) -> Result<Url, &'static str> {
    let decoded = BASE64.decode(encoded).map_err(|_| "invalid playlist")?;
    let playlist = String::from_utf8(decoded).map_err(|_| "invalid playlist")?;
    let mut lines = playlist.lines();
    if lines.next() != Some("#EXTM3U") || lines.next() != Some("#EXTINF:0") {
        return Err("invalid playlist");
    }
    let url = lines.next().ok_or("invalid playlist")?;
    if lines.next().is_some() {
        return Err("invalid playlist");
    }
    stream_url(url)
}

fn stream_url(input: &str) -> Result<Url, &'static str> {
    const ERROR: &str = "stream URL must be absolute HTTP(S)";
    if input.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return Err(ERROR);
    }
    let url = Url::parse(input).map_err(|_| ERROR)?;
    if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
        return Err(ERROR);
    }
    Ok(url)
}

fn spawn_player(
    name: &str,
    executables: &[&str],
    install_paths: &[&str],
    url: &Url,
    option_separator: bool,
) -> Result<(), &'static str> {
    let paths: Vec<PathBuf> = executables
        .iter()
        .flat_map(|exe| player_paths(exe, install_paths))
        .collect();
    let spawned = paths.iter().find_map(|path| {
        let mut command = Command::new(path);
        if option_separator {
            command.arg("--");
        }
        command
            .arg(url.as_str())
            .creation_flags(CREATE_BREAKAWAY_FROM_JOB)
            .spawn()
            .ok()
    });
    if spawned.is_none() {
        eprintln!("{name} was not found in {paths:?}");
        nwg::error_message(
            APP_NAME,
            &format!("{name} was not found on this computer. Install it and try again."),
        );
        return Err("external player is not installed");
    }
    Ok(())
}

fn player_paths(exe: &str, install_paths: &[&str]) -> Vec<PathBuf> {
    let app_paths = [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE]
        .iter()
        .filter_map(|hive| {
            RegKey::predef(*hive)
                .open_subkey(format!(r"{APP_PATHS}\{exe}"))
                .and_then(|key| key.get_value::<String, _>(""))
                .ok()
        })
        .map(|path| PathBuf::from(path.trim_matches('"')));
    let open_with = RegKey::predef(HKEY_CLASSES_ROOT)
        .open_subkey(format!(r"Applications\{exe}\shell\open\command"))
        .and_then(|key| key.get_value::<String, _>(""))
        .ok()
        .and_then(|command| command_program(&command));
    let default_dirs = ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"]
        .iter()
        .filter_map(env::var_os)
        .flat_map(|dir| {
            install_paths
                .iter()
                .map(move |path| PathBuf::from(&dir).join(path))
        });
    app_paths
        .chain(open_with)
        .chain(default_dirs)
        .chain([PathBuf::from(exe)])
        .collect()
}

fn command_program(command: &str) -> Option<PathBuf> {
    let program = match command.strip_prefix('"') {
        Some(quoted) => quoted.split('"').next()?,
        None => command.split(' ').next()?,
    };
    (!program.is_empty()).then(|| PathBuf::from(program))
}

fn open_playlist(url: &Url) -> Result<(), &'static str> {
    let directory = dirs::download_dir()
        .ok_or("Downloads directory is unavailable")?
        .join("Stremio");
    fs::create_dir_all(&directory).map_err(|_| "cannot create playlist directory")?;
    let path = directory.join("playlist.m3u");
    fs::write(&path, format!("#EXTM3U\n#EXTINF:0\n{url}"))
        .map_err(|_| "cannot save M3U playlist")?;
    // Native association opening treats the path as data, including %, &, and quotes.
    let path_wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            std::ptr::null(),
            path_wide.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    if result as isize > 32 {
        Ok(())
    } else {
        Err("cannot open M3U playlist")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STREAM_URL: &str = "https://example.com/video.mkv?token=secret%2Bvalue";

    fn data_uri(playlist: &str) -> String {
        format!("{}{}", M3U_DATA_URI_PREFIX, BASE64.encode(playlist))
    }

    #[test]
    fn accepts_core_forms() {
        let stream = Url::parse(STREAM_URL).unwrap();
        assert_eq!(
            parse(&format!("vlc://{}", STREAM_URL)),
            Ok(Request::Vlc(stream.clone()))
        );
        assert_eq!(
            parse(&format!("mpv://{}", STREAM_URL)),
            Ok(Request::Mpv(stream.clone()))
        );
        assert_eq!(
            parse(&format!("potplayer://{}", STREAM_URL)),
            Ok(Request::PotPlayer(stream.clone()))
        );
        assert_eq!(
            parse(&data_uri(&format!("#EXTM3U\n#EXTINF:0\n{}", STREAM_URL))),
            Ok(Request::Playlist(stream))
        );
    }

    #[test]
    fn reads_program_from_open_with_command() {
        assert_eq!(
            command_program(r#""C:\Program Files\VideoLAN\VLC\vlc.exe" "%1""#),
            Some(PathBuf::from(r"C:\Program Files\VideoLAN\VLC\vlc.exe"))
        );
        assert_eq!(
            command_program(r#""C:\VLC\vlc.exe" -- "%L""#),
            Some(PathBuf::from(r"C:\VLC\vlc.exe"))
        );
        assert_eq!(
            command_program(r"C:\VLC\vlc.exe %1"),
            Some(PathBuf::from(r"C:\VLC\vlc.exe"))
        );
        assert_eq!(command_program(""), None);
    }

    #[test]
    fn rejects_unsafe_values() {
        for input in [
            "",
            "calc",
            "file:///C:/Windows/System32/calc.exe",
            "mpv://file:///C:/Windows/System32/calc.exe",
            "vlc://file:///C:/Windows/System32/calc.exe",
            "vlc://https://example.com/video.mkv\n--script=attack.lua",
            "vlc://javascript:alert(1)",
            "potplayer://file:///C:/Windows/System32/calc.exe",
            "iina://weblink?url=https%3A%2F%2Fexample.com%2Fvideo.mkv",
            &data_uri("#EXTM3U\n#EXTINF:0\nfile:///C:/Windows/System32/calc.exe"),
            &data_uri("#EXTM3U\n#EXTINF:0\nhttps://example.com/one\nhttps://example.com/two"),
            &data_uri("#EXTM3U\n#EXTINF:-1\nhttps://example.com/video.mkv"),
            "data:application/octet-stream;charset=utf-8;base64,not base64",
        ] {
            assert!(parse(input).is_err(), "{}", input);
        }
    }
}
