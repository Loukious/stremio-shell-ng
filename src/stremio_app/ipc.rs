use serde::{Deserialize, Serialize};
use serde_json::{self, json};
use std::cell::RefCell;

const VERSION: &str = env!("CARGO_PKG_VERSION");

pub type Channel = RefCell<Option<(flume::Sender<String>, flume::Receiver<String>)>>;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RPCRequest {
    pub id: u64,
    pub args: Option<Vec<serde_json::Value>>,
}

impl RPCRequest {
    pub fn is_handshake(&self) -> bool {
        self.id == 0
    }
    pub fn get_method(&self) -> Option<&str> {
        self.args
            .as_ref()
            .and_then(|args| args.first())
            .and_then(|arg| arg.as_str())
    }
    pub fn get_params(&self) -> Option<&serde_json::Value> {
        self.args
            .as_ref()
            .and_then(|args| if args.len() > 1 { Some(&args[1]) } else { None })
    }
}
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RPCResponseDataTransport {
    pub properties: Vec<Vec<String>>,
    pub signals: Vec<String>,
    pub methods: Vec<Vec<String>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RPCResponseData {
    pub transport: RPCResponseDataTransport,
}

#[derive(Default, Serialize, Deserialize, Debug, Clone)]
pub struct RPCResponse {
    pub id: u64,
    pub object: String,
    #[serde(rename = "type")]
    pub response_type: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<RPCResponseData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<serde_json::Value>,
}

impl RPCResponse {
    pub fn get_handshake() -> String {
        let resp = RPCResponse {
            id: 0,
            object: "transport".to_string(),
            response_type: 3,
            data: Some(RPCResponseData {
                transport: RPCResponseDataTransport {
                    properties: vec![
                        vec![],
                        vec![
                            "".to_string(),
                            "shellVersion".to_string(),
                            "".to_string(),
                            VERSION.to_string(),
                        ],
                    ],
                    signals: vec![],
                    methods: vec![vec!["onEvent".to_string(), "".to_string()]],
                },
            }),
            ..Default::default()
        };
        serde_json::to_string(&resp).expect("Cannot build response")
    }
    pub fn response_message(msg: Option<serde_json::Value>) -> String {
        let resp = RPCResponse {
            id: 1,
            object: "transport".to_string(),
            response_type: 1,
            args: msg,
            ..Default::default()
        };
        serde_json::to_string(&resp).expect("Cannot build response")
    }
    pub fn visibility_change(visible: bool, visibility: u32, is_full_screen: bool) -> String {
        Self::response_message(Some(json!(["win-visibility-changed" ,{
            "visible": visible,
            "visibility": visibility,
            "isFullscreen": is_full_screen
        }])))
    }
    pub fn state_change(state: u32) -> String {
        Self::response_message(Some(json!(["win-state-changed" ,{
            "state": state,
        }])))
    }
    pub fn open_media(url: String) -> String {
        Self::response_message(Some(json!(["open-media", url])))
    }
}
