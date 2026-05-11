use crate::stremio_app::ipc;
use crate::stremio_app::RPCResponse;
use flume::{Receiver, Sender};
use libmpv2::events::PropertyData;
use libmpv2::{events::Event, Format, Mpv, SetData};
use native_windows_gui::{self as nwg, PartialUi};
use once_cell::sync::Lazy;
use std::sync::Mutex;
use std::{
    thread::{self, JoinHandle},
};
use winapi::shared::windef::HWND;

use crate::stremio_app::stremio_player::{
    CmdVal, InMsg, InMsgArgs, InMsgFn, MpvCmd, PlayerEnded, PlayerEvent, PlayerProprChange,
    PlayerResponse, PropKey, PropVal,
};

pub static CURRENT_TIME: Lazy<Mutex<f64>> = Lazy::new(|| Mutex::new(0.0));

pub static TOTAL_DURATION: Lazy<Mutex<f64>> = Lazy::new(|| Mutex::new(0.0));

pub static IS_PAUSED: Lazy<Mutex<bool>> = Lazy::new(|| Mutex::new(false));

pub static IS_FILE_LOADED: Lazy<Mutex<bool>> = Lazy::new(|| Mutex::new(false));

/// Current chapter index as reported by mpv (or -1 if unknown / no chapters).
pub static CURRENT_CHAPTER: Lazy<Mutex<i64>> = Lazy::new(|| Mutex::new(-1));

/// Current chapter title (from `chapter-metadata/by-key/title`).
pub static CURRENT_CHAPTER_TITLE: Lazy<Mutex<String>> = Lazy::new(|| Mutex::new(String::new()));

/// The actual video stream URL passed to `loadfile` (not the Stremio page URL).
pub static CURRENT_STREAM_URL: Lazy<Mutex<String>> = Lazy::new(|| Mutex::new(String::new()));

/// Global player command sender — allows the sync client (and other subsystems)
/// to inject mpv commands without going through the webview pipeline.
/// Populated once during `on_init`.
pub static PLAYER_CMD_TX: Lazy<Mutex<Option<Sender<String>>>> = Lazy::new(|| Mutex::new(None));

#[derive(Default)]
pub struct Player {
    pub channel: ipc::Channel,
}

impl PartialUi for Player {
    fn build_partial<W: Into<nwg::ControlHandle>>(
        // @TODO replace with `&mut self`?
        data: &mut Self,
        parent: Option<W>,
    ) -> Result<(), nwg::NwgError> {
        // @TODO replace all `expect`s with proper error handling?

        let parent = parent.ok_or_else(|| nwg::NwgError::no_parent("Player"))?;
        let window_handle = parent
            .into()
            .hwnd()
            .ok_or_else(|| nwg::NwgError::control_create("Cannot obtain player window handle"))?;

        let (in_msg_sender, in_msg_receiver) = flume::unbounded();
        let (rpc_response_sender, rpc_response_receiver) = flume::unbounded();
        data.channel = ipc::Channel::new(Some((in_msg_sender, rpc_response_receiver)));

        let mpv = create_shareable_mpv(window_handle);
        let _event_thread = create_event_thread(mpv, in_msg_receiver, rpc_response_sender);
        // @TODO implement a mechanism to stop threads on `Player` drop if needed

        Ok(())
    }
}

fn create_shareable_mpv(window_handle: HWND) -> Mpv {
    let mpv = Mpv::with_initializer(|initializer| {
        macro_rules! set_property {
            ($name:literal, $value:expr) => {
                initializer
                    .set_property($name, $value)
                    .expect(concat!("failed to set ", $name));
            };
        }
        set_property!("wid", window_handle as i64);
        set_property!("title", "Stremio");
        set_property!("audio-client-name", "Stremio");
        set_property!("config", "yes");
        set_property!("load-scripts", "yes");
        set_property!("terminal", "yes");
        #[cfg(debug_assertions)]
        set_property!("msg-level", "all=no,cplayer=debug");
        #[cfg(not(debug_assertions))]
        set_property!("msg-level", "all=no");
        set_property!("quiet", "yes");
        set_property!("hwdec", "auto");
        // set_property!("vo", "gpu-next,");
        Ok(())
    });
    mpv.expect("cannot build MPV")
}

fn create_event_thread(
    mut mpv: Mpv,
    in_msg_receiver: Receiver<String>,
    rpc_response_sender: Sender<String>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        mpv.disable_deprecated_events()
            .expect("failed to disable deprecated MPV events");

        for (name, format) in [
            ("time-pos", Format::Double),
            ("duration", Format::Double),
            ("pause", Format::Flag),
            ("chapter", Format::Int64),
            ("chapter-metadata/by-key/title", Format::String),
        ] {
            observe_property(&mpv, name, format);
        }

        loop {
            while let Ok(msg) = in_msg_receiver.try_recv() {
                handle_in_msg(&mpv, msg);
            }

            let event = match mpv.wait_event(0.01) {
                Some(Ok(event)) => event,
                Some(Err(error)) => {
                    eprintln!("Event errored: {error:?}");
                    continue;
                }
                None => continue,
            };

            let player_response = match event {
                Event::StartFile => {
                    *CURRENT_TIME.lock().unwrap() = 0.0;
                    *TOTAL_DURATION.lock().unwrap() = 0.0;
                    *IS_PAUSED.lock().unwrap() = true;
                    *IS_FILE_LOADED.lock().unwrap() = false;
                    *CURRENT_CHAPTER.lock().unwrap() = -1;
                    *CURRENT_CHAPTER_TITLE.lock().unwrap() = String::new();
                    continue;
                }
                Event::FileLoaded => {
                    *IS_FILE_LOADED.lock().unwrap() = true;
                    continue;
                }
                Event::PropertyChange { name, change, .. } => {
                    update_cached_property(&name, &change);
                    PlayerResponse(
                        "mpv-prop-change",
                        PlayerEvent::PropChange(PlayerProprChange::from_name_value(
                            name.to_string(),
                            change,
                        )),
                    )
                }
                Event::EndFile(reason) => {
                    *IS_FILE_LOADED.lock().unwrap() = false;
                    PlayerResponse(
                        "mpv-event-ended",
                        PlayerEvent::End(PlayerEnded::from_end_reason(reason)),
                    )
                }
                Event::Shutdown => {
                    break;
                }
                _ => continue,
            };

            rpc_response_sender
                .send(RPCResponse::response_message(player_response.to_value()))
                .expect("failed to send RPCResponse");
        }
    })
}

fn handle_in_msg(mpv: &Mpv, msg: String) {
    let in_msg: InMsg = match serde_json::from_str(&msg) {
        Ok(in_msg) => in_msg,
        Err(error) => {
            eprintln!("cannot parse InMsg:{:?} {error:#}", &msg);
            return;
        }
    };

    match in_msg {
        InMsg(InMsgFn::MpvObserveProp, InMsgArgs::ObProp(PropKey::Bool(prop))) => {
            observe_property(mpv, &prop.to_string(), Format::Flag);
        }
        InMsg(InMsgFn::MpvObserveProp, InMsgArgs::ObProp(PropKey::Int(prop))) => {
            observe_property(mpv, &prop.to_string(), Format::Int64);
        }
        InMsg(InMsgFn::MpvObserveProp, InMsgArgs::ObProp(PropKey::Fp(prop))) => {
            observe_property(mpv, &prop.to_string(), Format::Double);
        }
        InMsg(InMsgFn::MpvObserveProp, InMsgArgs::ObProp(PropKey::Str(prop))) => {
            observe_property(mpv, &prop.to_string(), Format::String);
        }
        InMsg(InMsgFn::MpvSetProp, InMsgArgs::StProp(name, PropVal::Bool(value))) => {
            set_property(name, value, mpv);
        }
        InMsg(InMsgFn::MpvSetProp, InMsgArgs::StProp(name, PropVal::Num(value))) => {
            set_property(name, value, mpv);
        }
        InMsg(InMsgFn::MpvSetProp, InMsgArgs::StProp(name, PropVal::Str(value))) => {
            let value = if name.to_string() == "sub-ass-override" && value == "strip" {
                // Map "strip" to "scale". This perfectly preserves ASS styles and positioning
                // but allows the subtitles to be scaled up/down.
                "scale".to_string()
            } else {
                value
            };
            set_property(name, value, mpv);
        }
        InMsg(InMsgFn::MpvCommand, InMsgArgs::Cmd(cmd)) => {
            // Capture the actual stream URL when loadfile is issued
            match &cmd {
                CmdVal::Double(MpvCmd::Loadfile, url)
                | CmdVal::Tripple(MpvCmd::Loadfile, url, ..)
                | CmdVal::Quadruple(MpvCmd::Loadfile, url, ..)
                | CmdVal::Quintuple(MpvCmd::Loadfile, url, ..) => {
                    if let Ok(mut stream_url) = CURRENT_STREAM_URL.lock() {
                        *stream_url = url.clone();
                        println!("📺 Stream URL captured: {}", url);
                    }
                }
                _ => {}
            }
            send_command(mpv, cmd);
        }
        msg => {
            eprintln!("MPV unsupported message: '{msg:?}'");
        }
    }
}

fn observe_property(mpv: &Mpv, name: &str, format: Format) {
    if let Err(error) = mpv.observe_property(name, format, 0) {
        eprintln!("failed to observe MPV property '{name}': {error:#}");
    }
}

fn update_cached_property(name: &str, change: &PropertyData) {
    if name == "time-pos" {
        if let PropertyData::Double(pos_secs) = change {
            *CURRENT_TIME.lock().unwrap() = *pos_secs;
        }
    }
    if name == "duration" {
        if let PropertyData::Double(dur_secs) = change {
            *TOTAL_DURATION.lock().unwrap() = *dur_secs;
        }
    }
    if name == "pause" {
        if let PropertyData::Flag(pause) = change {
            *IS_PAUSED.lock().unwrap() = *pause;
        }
    }

    if name == "chapter" {
        if let PropertyData::Int64(chapter_idx) = change {
            *CURRENT_CHAPTER.lock().unwrap() = *chapter_idx;
        }
    }
    if name == "chapter-metadata/by-key/title" {
        match change {
            PropertyData::Str(s) | PropertyData::OsdStr(s) => {
                *CURRENT_CHAPTER_TITLE.lock().unwrap() = s.to_string();
            }
            _ => {}
        }
    }
}

fn send_command(mpv: &Mpv, cmd: CmdVal) {
    let args: Vec<String>;
    let (name, args) = match cmd {
        CmdVal::Quintuple(name, arg1, arg2, arg3, arg4) => {
            args = vec![arg1, arg2, arg3, arg4];
            (name, args.iter().map(String::as_str).collect::<Vec<_>>())
        }
        CmdVal::Quadruple(name, arg1, arg2, arg3) => {
            args = vec![arg1, arg2, arg3];
            (name, args.iter().map(String::as_str).collect::<Vec<_>>())
        }
        CmdVal::Tripple(name, arg1, arg2) => {
            args = vec![arg1, arg2];
            (name, args.iter().map(String::as_str).collect::<Vec<_>>())
        }
        CmdVal::Double(name, arg1) => {
            args = vec![arg1];
            (name, args.iter().map(String::as_str).collect::<Vec<_>>())
        }
        CmdVal::Single((name,)) => (name, vec![]),
    };
    if let Err(error) = mpv.command(&name.to_string(), &args) {
        eprintln!("failed to execute MPV command: '{error:#}'")
    }
}

fn set_property(name: impl ToString, value: impl SetData, mpv: &Mpv) {
    if let Err(error) = mpv.set_property(&name.to_string(), value) {
        eprintln!("cannot set MPV property: '{error:#}'")
    }
}
