use crate::stremio_app::ipc;
use crate::stremio_app::RPCResponse;
use flume::{Receiver, Sender};
use libmpv2::events::PropertyData;
use libmpv2::{events::Event, Format, Mpv, SetData};
use native_windows_gui::{self as nwg, PartialUi};
use once_cell::sync::Lazy;
use std::sync::Mutex;
use std::{
    sync::Arc,
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

/// The actual video stream URL passed to `loadfile` (not the Stremio page URL).
pub static CURRENT_STREAM_URL: Lazy<Mutex<String>> = Lazy::new(|| Mutex::new(String::new()));

/// Global player command sender — allows the sync client (and other subsystems)
/// to inject mpv commands without going through the webview pipeline.
/// Populated once during `on_init`.
pub static PLAYER_CMD_TX: Lazy<Mutex<Option<Sender<String>>>> = Lazy::new(|| Mutex::new(None));

struct ObserveProperty {
    name: String,
    format: Format,
}

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
        let (observe_property_sender, observe_property_receiver) = flume::unbounded();
        data.channel = ipc::Channel::new(Some((in_msg_sender, rpc_response_receiver)));

        let mpv = create_shareable_mpv(window_handle);

        let _event_thread = create_event_thread(
            Arc::clone(&mpv),
            observe_property_receiver,
            rpc_response_sender,
        );
        let _message_thread = create_message_thread(mpv, observe_property_sender, in_msg_receiver);
        // @TODO implement a mechanism to stop threads on `Player` drop if needed

        Ok(())
    }
}

fn create_shareable_mpv(window_handle: HWND) -> Arc<Mutex<Mpv>> {
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
    Arc::new(Mutex::new(mpv.expect("cannot build MPV")))
}

fn create_event_thread(
    mpv: Arc<Mutex<Mpv>>,
    observe_property_receiver: Receiver<ObserveProperty>,
    rpc_response_sender: Sender<String>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        mpv.lock()
            .expect("MPV lock is poisoned")
            .disable_deprecated_events()
            .expect("failed to disable deprecated MPV events");

        loop {
            // Drain newly observed properties
            for ObserveProperty { name, format } in observe_property_receiver.drain() {
                mpv.lock()
                    .expect("MPV lock is poisoned")
                    .observe_property(&name, format, 0)
                    .expect("failed to observe MPV property");
            }

            let player_response = {
                let mut mpv = mpv.lock().expect("MPV lock is poisoned");
                let event = match mpv.wait_event(0.1) {
                    Some(Ok(event)) => event,
                    Some(Err(error)) => {
                        eprintln!("Event errored: {error:?}");
                        continue;
                    }
                    None => continue,
                };

                match event {
                    Event::StartFile => {
                        *CURRENT_TIME.lock().unwrap() = 0.0;
                        *TOTAL_DURATION.lock().unwrap() = 0.0;
                        *IS_PAUSED.lock().unwrap() = true;
                        *IS_FILE_LOADED.lock().unwrap() = false;
                        continue;
                    }
                    Event::FileLoaded => {
                        *IS_FILE_LOADED.lock().unwrap() = true;
                        continue;
                    }
                    Event::PropertyChange { name, change, .. } => {
                        // `change` is a plain `PropertyData`, not an Option
                        if name == "time-pos" {
                            // If it's a Double, print it
                            if let PropertyData::Double(pos_secs) = change {
                                *CURRENT_TIME.lock().unwrap() = pos_secs;
                            }
                        }
                        if name == "duration" {
                            if let PropertyData::Double(dur_secs) = change {
                                *TOTAL_DURATION.lock().unwrap() = dur_secs;
                            }
                        }
                        if name == "pause" {
                            if let PropertyData::Flag(pause) = change {
                                *IS_PAUSED.lock().unwrap() = pause;
                            }
                        }

                        // Because from_name_value expects `PropertyData`,
                        // just pass `change` directly:
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
                }
            };

            rpc_response_sender
                .send(RPCResponse::response_message(player_response.to_value()))
                .expect("failed to send RPCResponse");
        }
    })
}

fn create_message_thread(
    mpv: Arc<Mutex<Mpv>>,
    observe_property_sender: Sender<ObserveProperty>,
    in_msg_receiver: Receiver<String>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        {
            observe_property_sender
                .send(ObserveProperty {
                    name: "time-pos".to_string(),
                    format: Format::Double,
                })
                .expect("cannot send ObserveProperty");
            observe_property_sender
                .send(ObserveProperty {
                    name: "duration".to_string(),
                    format: Format::Double,
                })
                .expect("cannot send ObserveProperty");
            observe_property_sender
                .send(ObserveProperty {
                    name: "pause".to_string(),
                    format: Format::Flag,
                })
                .expect("cannot send ObserveProperty");
            mpv.lock().expect("MPV lock is poisoned").wake_up();
        }

        // -- Helpers --

        let observe_property = |name: String, format: Format| {
            observe_property_sender
                .send(ObserveProperty { name, format })
                .expect("cannot send ObserveProperty");
            mpv.lock().expect("MPV lock is poisoned").wake_up();
        };

        let send_command = |cmd: CmdVal| {
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
            if let Err(error) = mpv
                .lock()
                .expect("MPV lock is poisoned")
                .command(&name.to_string(), &args)
            {
                eprintln!("failed to execute MPV command: '{error:#}'")
            }
        };

        fn set_property(name: impl ToString, value: impl SetData, mpv: &Arc<Mutex<Mpv>>) {
            if let Err(error) = mpv
                .lock()
                .expect("MPV lock is poisoned")
                .set_property(&name.to_string(), value)
            {
                eprintln!("cannot set MPV property: '{error:#}'")
            }
        }

        // -- InMsg handler loop --

        for msg in in_msg_receiver.iter() {
            let in_msg: InMsg = match serde_json::from_str(&msg) {
                Ok(in_msg) => in_msg,
                Err(error) => {
                    eprintln!("cannot parse InMsg:{:?} {error:#}", &msg);
                    continue;
                }
            };

            match in_msg {
                InMsg(InMsgFn::MpvObserveProp, InMsgArgs::ObProp(PropKey::Bool(prop))) => {
                    observe_property(prop.to_string(), Format::Flag);
                }
                InMsg(InMsgFn::MpvObserveProp, InMsgArgs::ObProp(PropKey::Int(prop))) => {
                    observe_property(prop.to_string(), Format::Int64);
                }
                InMsg(InMsgFn::MpvObserveProp, InMsgArgs::ObProp(PropKey::Fp(prop))) => {
                    observe_property(prop.to_string(), Format::Double);
                }
                InMsg(InMsgFn::MpvObserveProp, InMsgArgs::ObProp(PropKey::Str(prop))) => {
                    observe_property(prop.to_string(), Format::String);
                }
                InMsg(InMsgFn::MpvSetProp, InMsgArgs::StProp(name, PropVal::Bool(value))) => {
                    set_property(name, value, &mpv);
                }
                InMsg(InMsgFn::MpvSetProp, InMsgArgs::StProp(name, PropVal::Num(value))) => {
                    set_property(name, value, &mpv);
                }
                InMsg(InMsgFn::MpvSetProp, InMsgArgs::StProp(name, PropVal::Str(value))) => {
                    let value = if name.to_string() == "sub-ass-override" && value == "strip" {
                        // Map "strip" to "scale". This perfectly preserves ASS styles and positioning
                        // but allows the subtitles to be scaled up/down.
                        "scale".to_string()
                    } else if name.to_string() == "vo" {
                        let mut value = value;
                        if !value.is_empty() && !value.ends_with(',') {
                            value.push(',');
                        }
                        value.push_str("gpu-next,");
                        value
                    } else {
                        value
                    };
                    set_property(name, value, &mpv);
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
                    send_command(cmd);
                }
                msg => {
                    eprintln!("MPV unsupported message: '{msg:?}'");
                }
            }
        }
    })
}

trait MpvExt {
    fn wake_up(&self);
}

impl MpvExt for Mpv {
    // @TODO create a PR to the `libmpv` crate and then remove `libmpv-sys` from Cargo.toml?
    fn wake_up(&self) {
        unsafe { libmpv2_sys::mpv_wakeup(self.ctx.as_ptr()) }
    }
}
