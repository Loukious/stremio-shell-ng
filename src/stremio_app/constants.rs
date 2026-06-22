pub const APP_NAME: &str = "Stremio";
pub const IPC_PATH: &str = "//./pipe/com.stremio5.";
pub const DEV_ENDPOINT: &str = "http://127.0.0.1:11470";
pub const WEB_ENDPOINT: &str = "https://web.stremio.com/";
pub const STA_ENDPOINT: &str = "https://staging.strem.io/";
pub const WINDOW_MIN_WIDTH: i32 = 1000;
pub const WINDOW_MIN_HEIGHT: i32 = 600;
pub const UPDATE_INTERVAL: u64 = 12 * 60 * 60;
pub const UPDATE_ENDPOINT: &str =
    "https://api.github.com/repos/Loukious/stremio-shell-ng/releases/latest";
pub const STREMIO_SERVER_DEV_MODE: &str = "STREMIO_SERVER_DEV_MODE";
pub const SRV_BUFFER_SIZE: usize = 1024;
pub const SERVER_IPC_KEY: &str = "SERVER_IPC_KEY";
pub const SRV_LOG_SIZE: usize = 20;

pub fn web_endpoint_with_streaming_server(server_url: &str) -> String {
    let server_url = server_url.trim_end_matches('/');
    let streaming_server_url =
        url::form_urlencoded::byte_serialize(server_url.as_bytes()).collect::<String>();
    let web_endpoint = WEB_ENDPOINT.trim_end_matches('/');

    format!("{web_endpoint}/#/?streamingServerUrl={streaming_server_url}")
}
