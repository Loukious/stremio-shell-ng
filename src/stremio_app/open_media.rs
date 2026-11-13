use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use url::Url;

const MAX_TORRENT_SIZE: u64 = 16 * 1024 * 1024;

// Resolve relative CLI paths before forwarding to an instance with a different cwd.
pub fn normalize(input: String) -> String {
    let path = Path::new(&input);
    if path.is_absolute() || Url::parse(&input).is_err() {
        if let Ok(path) = std::path::absolute(path) {
            if let Ok(url) = Url::from_file_path(path) {
                return url.into();
            }
        }
    }
    input
}

// Called on a native background thread, never from the window or WebView callback.
pub fn message(input: &str) -> Value {
    match open(input) {
        Ok(message) => message,
        Err(error) => json!(["open-error", error]),
    }
}

fn open(input: &str) -> Result<Value, String> {
    let url = Url::parse(input).map_err(|_| "Invalid media link.".to_string())?;
    let path: PathBuf = match url.scheme() {
        "stremio" | "magnet" | "http" | "https" => return Ok(json!(["open-media", input])),
        "file" => url
            .to_file_path()
            .map_err(|_| "Invalid local file URL.".to_string())?,
        _ => return Err("Unsupported media link.".to_string()),
    };
    if !path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("torrent"))
    {
        return Ok(json!(["open-media", input]));
    }
    let metadata = path
        .metadata()
        .map_err(|error| format!("Cannot open torrent file: {error}"))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_TORRENT_SIZE {
        return Err("Torrent file must be between 1 byte and 16 MiB.".to_string());
    }
    let mut data = Vec::new();
    File::open(&path)
        .and_then(|file| file.take(MAX_TORRENT_SIZE + 1).read_to_end(&mut data))
        .map_err(|error| format!("Cannot read torrent file: {error}"))?;
    if data.is_empty() || data.len() as u64 > MAX_TORRENT_SIZE {
        return Err("Torrent file must be between 1 byte and 16 MiB.".to_string());
    }
    let name = path
        .file_name()
        .ok_or_else(|| "Invalid torrent file name.".to_string())?
        .to_string_lossy();
    Ok(json!(["open-torrent", {"name": name, "data": STANDARD.encode(data)}]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_links_and_encoded_trackers() {
        for input in [
            "stremio://example.com/manifest.json",
            "stremio:///detail/movie/tt1234567",
            "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&tr=https%3A%2F%2Fexample.com%2Fa%3Fx%3D1%26y%3D2",
        ] {
            assert_eq!(message(input), json!(["open-media", input]));
        }
    }

    #[test]
    fn opens_torrent_bytes_from_an_encoded_file_url() {
        let directory = std::env::temp_dir().join(format!("stremio-open-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("movie #1 % & 漢字.TORRENT");
        let data = b"d4:infod4:name5:movieee";
        std::fs::write(&path, data).unwrap();
        let input = normalize(path.to_str().unwrap().to_string());
        assert_eq!(Url::parse(&input).unwrap().to_file_path().unwrap(), path);
        assert_eq!(
            message(&input),
            json!(["open-torrent", {
                "name": "movie #1 % & 漢字.TORRENT", "data": STANDARD.encode(data)
            }])
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rejects_other_schemes_and_files() {
        for input in ["javascript:alert(1)", "data:text/html,hello"] {
            assert!(open(input).is_err());
        }
    }

    #[test]
    fn preserves_direct_and_local_media_handoff() {
        for input in [
            "https://example.com/movie.mp4",
            "file:///C:/Videos/movie.mkv",
        ] {
            assert_eq!(message(input), json!(["open-media", input]));
        }
    }

    #[test]
    fn rejects_oversized_torrents_before_reading() {
        let path = std::env::temp_dir().join(format!("{}.torrent", uuid::Uuid::new_v4()));
        File::create(&path)
            .unwrap()
            .set_len(MAX_TORRENT_SIZE + 1)
            .unwrap();
        let input = Url::from_file_path(&path).unwrap();
        assert!(open(input.as_str()).unwrap_err().contains("16 MiB"));
        std::fs::remove_file(path).unwrap();
    }
}
