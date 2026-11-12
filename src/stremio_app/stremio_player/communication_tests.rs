use crate::stremio_app::stremio_player::communication::{
    BoolProp, CmdVal, InMsg, InMsgArgs, InMsgFn, MpvCmd, PlayerEnded, PlayerProprChange,
    PlayerResponse, PropKey, PropVal,
};
use libmpv2::{events::PropertyData, mpv_end_file_reason};

use serde_test::{assert_tokens, Token};

#[test]
fn propr_change_tokens() {
    let prop = "test-prop";
    let tokens: [Token; 6] = [
        Token::Struct {
            name: "PlayerProprChange",
            len: 2,
        },
        Token::Str("name"),
        Token::None,
        Token::Str("data"),
        Token::None,
        Token::StructEnd,
    ];

    fn tokens_by_type(tokens: &[Token; 6], name: &'static str, val: PropertyData, token: Token) {
        let mut typed_tokens = tokens.clone();
        typed_tokens[2] = Token::Str(name);
        typed_tokens[4] = token;
        assert_tokens(
            &PlayerProprChange::from_name_value(name.to_string(), val),
            &typed_tokens,
        );
    }
    tokens_by_type(&tokens, prop, PropertyData::Flag(true), Token::Bool(true));
    tokens_by_type(&tokens, prop, PropertyData::Int64(1), Token::F64(1.0));
    tokens_by_type(&tokens, prop, PropertyData::Double(1.0), Token::F64(1.0));
    tokens_by_type(&tokens, prop, PropertyData::OsdStr("ok"), Token::Str("ok"));
    tokens_by_type(&tokens, prop, PropertyData::Str("ok"), Token::Str("ok"));

    // JSON response
    tokens_by_type(
        &tokens,
        "track-list",
        PropertyData::Str(r#""ok""#),
        Token::Str("ok"),
    );
    tokens_by_type(
        &tokens,
        "video-params",
        PropertyData::Str(r#""ok""#),
        Token::Str("ok"),
    );
    tokens_by_type(
        &tokens,
        "metadata",
        PropertyData::Str(r#""ok""#),
        Token::Str("ok"),
    );
}

#[test]
fn ended_tokens() {
    let error_tokens: [Token; 12] = [
        Token::Struct {
            name: "PlayerEnded",
            len: 2,
        },
        Token::Str("reason"),
        Token::Str("error"),
        Token::Str("error"),
        Token::Some,
        Token::Struct {
            name: "PlayerEndedError",
            len: 2,
        },
        Token::Str("message"),
        Token::Str("Unknown error"),
        Token::Str("critical"),
        Token::Bool(false),
        Token::StructEnd,
        Token::StructEnd,
    ];
    let tokens: [Token; 4] = [
        Token::Struct {
            name: "PlayerEnded",
            len: 1,
        },
        Token::Str("reason"),
        Token::Str("quit"),
        Token::StructEnd,
    ];
    assert_tokens(
        &PlayerEnded::from_end_reason(mpv_end_file_reason::Error),
        &error_tokens,
    );
    assert_tokens(
        &PlayerEnded::from_end_reason(mpv_end_file_reason::Quit),
        &tokens,
    );
    let eof_tokens: [Token; 4] = [
        Token::Struct {
            name: "PlayerEnded",
            len: 1,
        },
        Token::Str("reason"),
        Token::Str("eof"),
        Token::StructEnd,
    ];
    assert_tokens(
        &PlayerEnded::from_end_reason(mpv_end_file_reason::Eof),
        &eof_tokens,
    );
    let stop_tokens: [Token; 4] = [
        Token::Struct {
            name: "PlayerEnded",
            len: 1,
        },
        Token::Str("reason"),
        Token::Str("stop"),
        Token::StructEnd,
    ];
    assert_tokens(
        &PlayerEnded::from_end_reason(mpv_end_file_reason::Stop),
        &stop_tokens,
    );
}

#[test]
fn ob_propr_tokens() {
    assert_tokens(
        &InMsg(
            InMsgFn::MpvObserveProp,
            InMsgArgs::ObProp(PropKey::Bool(BoolProp::Pause)),
        ),
        &[
            Token::TupleStruct {
                name: "InMsg",
                len: 2,
            },
            Token::Str("mpv-observe-prop"),
            Token::Str("pause"),
            Token::TupleStructEnd,
        ],
    );
}

#[test]
fn set_propr_tokens() {
    assert_tokens(
        &InMsg(
            InMsgFn::MpvSetProp,
            InMsgArgs::StProp(PropKey::Bool(BoolProp::Pause), PropVal::Bool(true)),
        ),
        &[
            Token::TupleStruct {
                name: "InMsg",
                len: 2,
            },
            Token::Str("mpv-set-prop"),
            Token::Tuple { len: 2 },
            Token::Str("pause"),
            Token::Bool(true),
            Token::TupleEnd,
            Token::TupleStructEnd,
        ],
    );
}

#[test]
fn set_gpu_video_processing_tokens() {
    assert_tokens(
        &InMsg(InMsgFn::MpvSetGpuVideoProcessing, InMsgArgs::Flag(true)),
        &[
            Token::TupleStruct {
                name: "InMsg",
                len: 2,
            },
            Token::Str("mpv-set-gpu-video-processing"),
            Token::Bool(true),
            Token::TupleStructEnd,
        ],
    );
}

#[test]
fn command_stop_tokens() {
    assert_eq!(
        serde_json::to_value(&InMsg(
            InMsgFn::MpvCommand,
            InMsgArgs::Cmd(CmdVal::Single((MpvCmd::Stop,))),
        ))
        .unwrap(),
        serde_json::json!(["mpv-command", ["stop"]])
    );
}

#[test]
fn command_loadfile_tokens() {
    assert_eq!(
        serde_json::to_value(&InMsg(
            InMsgFn::MpvCommand,
            InMsgArgs::Cmd(CmdVal::Double(MpvCmd::Loadfile, "some_file".to_string())),
        ))
        .unwrap(),
        serde_json::json!(["mpv-command", ["loadfile", "some_file"]])
    );
}

#[test]
fn loadfile_accepts_fractional_start_and_rejects_other_options() {
    for parts in [
        serde_json::json!(["loadfile", "video", "replace", "start=+12.5"]),
        serde_json::json!(["loadfile", "video", "replace", "-1", "start=+12.5"]),
    ] {
        let command = serde_json::from_value::<CmdVal>(parts.clone()).unwrap();
        assert_eq!(serde_json::to_value(command).unwrap(), parts);
    }
    for parts in [
        serde_json::json!(["loadfile"]),
        serde_json::json!(["loadfile", "video", "append"]),
        serde_json::json!(["loadfile", "video", "replace", "0", "start=+1"]),
        serde_json::json!(["loadfile", "video", "replace", "stream-dump=output"]),
        serde_json::json!([
            "loadfile",
            "video",
            "replace",
            "start=+1,stream-dump=output"
        ]),
        serde_json::json!(["loadfile", "video", "replace", "start=+NaN"]),
        serde_json::json!(["loadfile", "video", "replace", "start=+inf"]),
        serde_json::json!(["loadfile", "video", "replace", "start=+-1"]),
    ] {
        assert!(
            serde_json::from_value::<InMsg>(serde_json::json!(["mpv-command", parts])).is_err()
        );
    }
}

#[test]
fn subtitle_commands_use_deferred_selection_and_explicit_track_ids() {
    for parts in [
        serde_json::json!([
            "sub-add",
            "https://example.com/a.ass",
            "auto",
            "English",
            "eng"
        ]),
        serde_json::json!(["sub-remove", "7"]),
    ] {
        let command = serde_json::from_value::<CmdVal>(parts.clone()).unwrap();
        assert_eq!(serde_json::to_value(command).unwrap(), parts);
    }
    for parts in [
        serde_json::json!(["sub-add", "https://example.com/a.ass"]),
        serde_json::json!([
            "sub-add",
            "https://example.com/a.ass",
            "select",
            "English",
            "eng"
        ]),
        serde_json::json!(["sub-remove"]),
        serde_json::json!(["sub-remove", "0"]),
        serde_json::json!(["sub-remove", "-1"]),
    ] {
        assert!(serde_json::from_value::<CmdVal>(parts).is_err());
    }
}

#[test]
fn internal_player_commands_keep_their_existing_wire_shapes() {
    for parts in [
        serde_json::json!(["add", "chapter", "1"]),
        serde_json::json!(["cycle", "pause"]),
        serde_json::json!(["seek", "12.5", "absolute"]),
        serde_json::json!(["keypress", "Ctrl+LEFT"]),
    ] {
        let command = serde_json::from_value::<CmdVal>(parts.clone()).unwrap();
        assert_eq!(serde_json::to_value(command).unwrap(), parts);
    }
}

#[test]
fn video_ready_reports_the_load_id_in_the_frontend_protocol() {
    assert_eq!(
        PlayerResponse::video_ready(42, true).to_value(),
        Some(serde_json::json!(["mpv-event-video-ready", {"loadId":42,"ready":true}]))
    );
}
