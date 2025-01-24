use core::convert::TryFrom;
use libmpv2::{events::PropertyData, mpv_end_file_reason, EndFileReason};
use parse_display::{Display, FromStr};
use serde::{Deserialize, Serialize};
use std::fmt;

// Responses
const JSON_RESPONSES: [&str; 3] = ["track-list", "video-params", "metadata"];

#[derive(Serialize, Deserialize, Debug, Clone, Eq, PartialEq)]
pub struct PlayerProprChange {
    name: String,
    data: serde_json::Value,
}
impl PlayerProprChange {
    fn value_from_format(data: PropertyData, as_json: bool) -> serde_json::Value {
        match data {
            PropertyData::Flag(d) => serde_json::Value::Bool(d),
            PropertyData::Int64(d) => serde_json::Value::Number(
                serde_json::Number::from_f64(d as f64).expect("MPV returned invalid number"),
            ),
            PropertyData::Double(d) => serde_json::Value::Number(
                serde_json::Number::from_f64(d).expect("MPV returned invalid number"),
            ),
            PropertyData::OsdStr(s) => serde_json::Value::String(s.to_string()),
            PropertyData::Str(s) => {
                if as_json {
                    serde_json::from_str(s).expect("MPV returned invalid JSON data")
                } else {
                    serde_json::Value::String(s.to_string())
                }
            }
            PropertyData::Node(_) => unimplemented!("`PropertyData::Node` is not supported"),
        }
    }
    pub fn from_name_value(name: String, value: PropertyData) -> Self {
        let is_json = JSON_RESPONSES.contains(&name.as_str());
        Self {
            name,
            data: Self::value_from_format(value, is_json),
        }
    }
}
#[derive(Serialize, Deserialize, Debug, Clone, Eq, PartialEq)]
pub struct PlayerEnded {
    reason: String,
}
impl PlayerEnded {
    fn string_from_end_reason(data: EndFileReason) -> String {
        match data {
            mpv_end_file_reason::Error => "error".to_string(),
            mpv_end_file_reason::Quit => "quit".to_string(),
            _ => "other".to_string(),
        }
    }
    pub fn from_end_reason(data: EndFileReason) -> Self {
        Self {
            reason: Self::string_from_end_reason(data),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PlayerError {
    pub error: String,
}
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum PlayerEvent {
    PropChange(PlayerProprChange),
    End(PlayerEnded),
    Error(PlayerError),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PlayerResponse<'a>(pub &'a str, pub PlayerEvent);
impl PlayerResponse<'_> {
    pub fn to_value(&self) -> Option<serde_json::Value> {
        serde_json::to_value(self).ok()
    }
}

// Player incoming messages from the web UI
/*
Message general case - ["function-name", ["arguments", ...]]
The function could be either mpv-observe-prop, mpv-set-prop or mpv-command.

["mpv-observe-prop", "prop-name"]
["mpv-set-prop", ["prop-name", prop-val]]
["mpv-command", ["command-name"<, "arguments">]]

All the function and property names are in kebab-case.

MPV requires type for any prop-name when observing or setting it's value.
The type for setting is not always the same as the type for observing the prop.

"mpv-observe-prop" function is the only one that accepts single string
instead of array of arguments

"mpv-command" function always takes an array even if the command doesn't
have any arguments. For example this are the commands we support:

["mpv-command", ["loadfile", "file name"]]
["mpv-command", ["stop"]]
*/
macro_rules! stringable {
    ($t:ident) => {
        impl From<$t> for String {
            fn from(s: $t) -> Self {
                s.to_string()
            }
        }
        impl TryFrom<String> for $t {
            type Error = parse_display::ParseError;
            fn try_from(s: String) -> Result<Self, Self::Error> {
                s.parse()
            }
        }
    };
}

#[allow(clippy::enum_variant_names)]
#[derive(Display, FromStr, Serialize, Deserialize, Debug, Clone, Eq, PartialEq)]
#[serde(try_from = "String", into = "String")]
#[display(style = "kebab-case")]
pub enum InMsgFn {
    MpvSetProp,
    MpvCommand,
    MpvObserveProp,
}
stringable!(InMsgFn);
// Bool
#[derive(Display, FromStr, Serialize, Deserialize, Debug, Clone, Eq, PartialEq)]
#[serde(try_from = "String", into = "String")]
#[display(style = "kebab-case")]
pub enum BoolProp {
    Pause,
    PausedForCache,
    Seeking,
    EofReached,
}
stringable!(BoolProp);
// Int
#[derive(Display, FromStr, Serialize, Deserialize, Debug, Clone, Eq, PartialEq)]
#[serde(try_from = "String", into = "String")]
#[display(style = "kebab-case")]
pub enum IntProp {
    Aid,
    Vid,
    Sid,
}
stringable!(IntProp);
// Fp
#[derive(Display, FromStr, Serialize, Deserialize, Debug, Clone, Eq, PartialEq)]
#[serde(try_from = "String", into = "String")]
#[display(style = "kebab-case")]
pub enum FpProp {
    TimePos,
    Mute,
    Volume,
    Duration,
    SubScale,
    CacheBufferingState,
    SubPos,
    Speed,
}
stringable!(FpProp);
// Str
#[derive(Display, FromStr, Serialize, Deserialize, Debug, Clone, Eq, PartialEq)]
#[serde(try_from = "String", into = "String")]
#[display(style = "kebab-case")]
pub enum StrProp {
    FfmpegVersion,
    Hwdec,
    InputDefaltBindings,
    InputVoKeyboard,
    Metadata,
    MpvVersion,
    Osc,
    Path,
    SubAssOverride,
    SubBackColor,
    SubBorderColor,
    SubColor,
    TrackList,
    VideoParams,
    // Vo,
}
stringable!(StrProp);

// Any
#[derive(Serialize, Deserialize, Debug, Clone, Eq, PartialEq)]
#[serde(untagged)]
pub enum PropKey {
    Bool(BoolProp),
    Int(IntProp),
    Fp(FpProp),
    Str(StrProp),
}
impl fmt::Display for PropKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Bool(v) => write!(f, "{}", v),
            Self::Int(v) => write!(f, "{}", v),
            Self::Fp(v) => write!(f, "{}", v),
            Self::Str(v) => write!(f, "{}", v),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(untagged)]
pub enum PropVal {
    Bool(bool),
    Str(String),
    Num(f64),
}

#[derive(Display, FromStr, Serialize, Deserialize, Debug, Clone, Eq, PartialEq)]
#[serde(try_from = "String", into = "String")]
#[display(style = "kebab-case")]
#[serde(untagged)]
pub enum MpvCmd {
    Loadfile,
    Stop,
    Cycle,
    Seek,
}
stringable!(MpvCmd);

#[derive(Serialize, Deserialize, Debug, Clone, Eq, PartialEq)]
#[serde(untagged)]
pub enum CmdVal {
    Single((MpvCmd,)),
    Double(MpvCmd, String),
    Tripple(MpvCmd, String, String),
    Quadruple(MpvCmd, String, String, String),
    Quintuple(MpvCmd, String, String, String, String),
}
impl From<CmdVal> for Vec<String> {
    fn from(cmd: CmdVal) -> Vec<String> {
        match cmd {
            CmdVal::Single(cmd) => vec![cmd.0.to_string()],
            CmdVal::Double(cmd, arg) => vec![cmd.to_string(), arg],
            CmdVal::Tripple(cmd, arg1, arg2) => vec![cmd.to_string(), arg1, arg2],
            CmdVal::Quadruple(cmd, arg1, arg2, arg3) => vec![cmd.to_string(), arg1, arg2, arg3],
            CmdVal::Quintuple(cmd, arg1, arg2, arg3, arg4) => {
                vec![cmd.to_string(), arg1, arg2, arg3, arg4]
            }
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(untagged)]
pub enum InMsgArgs {
    StProp(PropKey, PropVal),
    Cmd(CmdVal),
    ObProp(PropKey),
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct InMsg(pub InMsgFn, pub InMsgArgs);
