use serde::Deserialize;
use serde_json::json;
use url::Url;

use crate::stremio_app::ipc::RPCResponse;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheDirectoryRequest {
    pub request_id: u64,
    pub server_url: String,
    pub directory: Option<String>,
}

pub fn local_server_url(input: &str) -> Option<Url> {
    let url = Url::parse(input).ok()?;
    let loopback = match url.host()? {
        url::Host::Domain(domain) => domain.eq_ignore_ascii_case("localhost"),
        url::Host::Ipv4(address) => address.is_loopback(),
        url::Host::Ipv6(address) => address.is_loopback(),
    };
    (loopback
        && matches!(url.scheme(), "http" | "https")
        && url.username().is_empty()
        && url.password().is_none())
    .then_some(url)
}

pub fn matches_local_server(requested: &str, running: Option<&str>) -> bool {
    let requested = local_server_url(requested);
    let running = running.and_then(local_server_url);
    requested.is_some() && requested == running
}

pub fn selected(request_id: u64, result: Result<Option<String>, String>) -> String {
    let (path, error) = match result {
        Ok(path) => (path, None),
        Err(error) => (None, Some(error)),
    };
    RPCResponse::response_message(Some(json!(["cache-directory-selected", {
        "requestId": request_id,
        "path": path,
        "error": error,
    }])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_the_current_loopback_server() {
        assert!(matches_local_server(
            "http://127.0.0.1:1234/",
            Some("http://127.0.0.1:1234")
        ));
        assert!(matches_local_server(
            "http://[::1]:1234",
            Some("http://[::1]:1234")
        ));
        for candidate in [
            "http://127.0.0.1:1235",
            "http://example.com:1234",
            "http://127.0.0.1:1234/path",
            "http://name@127.0.0.1:1234",
        ] {
            assert!(!matches_local_server(
                candidate,
                Some("http://127.0.0.1:1234")
            ));
        }
        assert!(!matches_local_server("http://127.0.0.1:1234", None));
    }

    #[test]
    fn reports_selection_cancellation_and_errors() {
        for result in [
            Ok(Some("C:\\Media & Cache".to_owned())),
            Ok(None),
            Err("busy".to_owned()),
        ] {
            let response: serde_json::Value =
                serde_json::from_str(&selected(42, result.clone())).unwrap();
            assert_eq!(response["args"][0], "cache-directory-selected");
            assert_eq!(response["args"][1]["requestId"], 42);
            match result {
                Ok(path) => {
                    assert_eq!(response["args"][1]["path"], json!(path));
                    assert!(response["args"][1]["error"].is_null());
                }
                Err(error) => {
                    assert!(response["args"][1]["path"].is_null());
                    assert_eq!(response["args"][1]["error"], error);
                }
            }
        }
    }
}
