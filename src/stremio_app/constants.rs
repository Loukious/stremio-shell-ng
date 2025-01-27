pub const APP_NAME: &str = "Stremio";
pub const IPC_PATH: &str = "//./pipe/com.stremio5.";
pub const DEV_ENDPOINT: &str = "http://127.0.0.1:11470";
pub const WEB_ENDPOINT: &str = "https://web.stremio.com/";
pub const STA_ENDPOINT: &str = "https://staging.strem.io/";
pub const WINDOW_MIN_WIDTH: i32 = 1000;
pub const WINDOW_MIN_HEIGHT: i32 = 600;
pub const UPDATE_INTERVAL: u64 = 12 * 60 * 60;
pub const UPDATE_ENDPOINT: [&str; 3] = [
    "https://www.strem.io/updater/check?product=stremio-shell-ng",
    "https://www.stremio.com/updater/check?product=stremio-shell-ng",
    "https://www.stremio.net/updater/check?product=stremio-shell-ng",
];
pub const STREMIO_SERVER_DEV_MODE: &str = "STREMIO_SERVER_DEV_MODE";
pub const SRV_BUFFER_SIZE: usize = 1024;
pub const SRV_LOG_SIZE: usize = 20;
