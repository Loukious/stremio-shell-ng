use std::time::Duration;
use url::Url;

pub const APP_NAME: &str = "Stremio";
pub const IPC_PATH: &str = "//./pipe/com.stremio5.";
pub const IPC_ATTEMPTS: u32 = 10;
pub const IPC_RETRY_DELAY: Duration = Duration::from_millis(100);
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

pub const WARNING_URL: &str = "https://www.stremio.com/warning#";
pub const WHITELISTED_HOSTS: &[&str] = &[
    "stremio.com",
    "www.stremio.com",
    "web.stremio.com",
    "app.stremio.com",
    "strem.io",
    "api.strem.io",
    "stremio.zendesk.com",
    "google.com",
    "www.google.com",
    "youtube.com",
    "www.youtube.com",
    "twitch.tv",
    "twitter.com",
    "x.com",
    "netflix.com",
    "adex.network",
    "amazon.com",
    "forms.gle",
    "www.hbomax.com",
    "play.hbomax.com",
    "www.disneyplus.com",
    "imdb.com",
];

pub fn safe_url(uri: &str) -> Option<String> {
    if let Ok(url) = Url::parse(uri) {
        // Local files belong to the drag and drop handler, never to the browser.
        // Windows paths such as C:\... parse as a one-letter scheme.
        if url.scheme() == "file" || url.scheme().len() == 1 {
            return None;
        }
        let is_whitelisted = url.host().is_some_and(|host| {
            WHITELISTED_HOSTS
                .iter()
                .any(|whitelisted_host| host.to_string() == *whitelisted_host)
        });

        let final_url = if is_whitelisted {
            url.to_string()
        } else {
            format!("{}{}", WARNING_URL, urlencoding::encode(url.as_ref()))
        };
        Some(final_url)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_file_urls_are_not_opened_externally() {
        for uri in [
            "file:///C:/Users/Test%20User/subtitle.srt",
            "file:///D:/a/_temp/Test%20User/legenda%20%C3%A7%C3%A3o.srt",
            "FILE:///C:/Users/Test/subtitle.srt",
            "file://server/share/subtitle.srt",
        ] {
            assert_eq!(safe_url(uri), None, "{uri}");
        }
    }

    #[test]
    fn windows_paths_are_not_opened_externally() {
        for uri in [
            r"C:\Users\Test User\subtitle.srt",
            "C:/Users/Test User/legenda ção.srt",
        ] {
            assert_eq!(safe_url(uri), None, "{uri}");
        }
    }

    #[test]
    fn whitelisted_links_open_directly() {
        assert_eq!(
            safe_url("https://www.stremio.com/"),
            Some("https://www.stremio.com/".to_string())
        );
    }

    #[test]
    fn other_links_open_the_warning_page() {
        assert_eq!(
            safe_url("https://example.com/page?a=1"),
            Some(format!(
                "{WARNING_URL}{}",
                urlencoding::encode("https://example.com/page?a=1")
            ))
        );
        assert_eq!(
            safe_url("data:text/plain,hello"),
            Some(format!(
                "{WARNING_URL}{}",
                urlencoding::encode("data:text/plain,hello")
            ))
        );
    }

    #[test]
    fn invalid_urls_are_not_opened() {
        assert_eq!(safe_url("not a url"), None);
    }
}
