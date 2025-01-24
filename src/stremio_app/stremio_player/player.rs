use crate::stremio_app::ipc;
use crate::stremio_app::RPCResponse;
use flume::{Receiver, Sender};
use libmpv2::events::PropertyData;
use libmpv2::{events::Event, events::EventContext, Format, Mpv, SetData};
use native_windows_gui::{self as nwg, PartialUi};
use once_cell::sync::Lazy;
use std::sync::Mutex;
use std::{
    sync::Arc,
    thread::{self, JoinHandle},
};
use winapi::shared::windef::HWND;

use crate::stremio_app::stremio_player::{
    CmdVal, InMsg, InMsgArgs, InMsgFn, PlayerEnded, PlayerEvent, PlayerProprChange, PlayerResponse,
    PropKey, PropVal,
};

pub static CURRENT_TIME: Lazy<Mutex<f64>> = Lazy::new(|| Mutex::new(0.0));

pub static TOTAL_DURATION: Lazy<Mutex<f64>> = Lazy::new(|| Mutex::new(0.0));

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

        let window_handle = parent
            .expect("no parent window")
            .into()
            .hwnd()
            .expect("cannot obtain window handle");

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

fn create_shareable_mpv(window_handle: HWND) -> Arc<Mpv> {
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
        set_property!("config", "yes");
        set_property!("load-scripts", "yes");
        set_property!("terminal", "yes");
        set_property!("msg-level", "all=no,cplayer=debug");
        set_property!("quiet", "yes");
        set_property!("hwdec", "auto");
        // FIXME: very often the audio track isn't selected when using "aid" = "auto"
        set_property!("aid", "1");
        Ok(())
    });
    Arc::new(mpv.expect("cannot build MPV"))
}

fn create_event_thread(
    mpv: Arc<Mpv>,
    observe_property_receiver: Receiver<ObserveProperty>,
    rpc_response_sender: Sender<String>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut event_context = EventContext::new(mpv.ctx);
        event_context
            .disable_deprecated_events()
            .expect("failed to disable deprecated MPV events");

        loop {
            // Drain newly observed properties
            for ObserveProperty { name, format } in observe_property_receiver.drain() {
                event_context
                    .observe_property(&name, format, 0)
                    .expect("failed to observe MPV property");
            }

            let event = match event_context.wait_event(-1.) {
                Some(Ok(event)) => event,
                Some(Err(error)) => {
                    eprintln!("Event errored: {error:?}");
                    continue;
                }
                None => continue,
            };

            let player_response = match event {
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
                Event::EndFile(reason) => PlayerResponse(
                    "mpv-event-ended",
                    PlayerEvent::End(PlayerEnded::from_end_reason(reason)),
                ),
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

fn create_message_thread(
    mpv: Arc<Mpv>,
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
            mpv.wake_up();
            observe_property_sender
                .send(ObserveProperty {
                    name: "duration".to_string(),
                    format: Format::Double,
                })
                .expect("cannot send ObserveProperty");
        }

        // -- Helpers --

        let observe_property = |name: String, format: Format| {
            observe_property_sender
                .send(ObserveProperty { name, format })
                .expect("cannot send ObserveProperty");
            mpv.wake_up();
        };

        let send_command = |cmd: CmdVal| {
            let a1;
            let a2;
            let a3;
            let a4;
            let (name, args) = match cmd {
                CmdVal::Quintuple(name, arg1, arg2, arg3, arg4) => {
                    a1 = format!(r#""{arg1}""#);
                    a2 = format!(r#""{arg2}""#);
                    a3 = format!(r#""{arg3}""#);
                    a4 = format!(r#""{arg4}""#);
                    (
                        name,
                        vec![a1.as_ref(), a2.as_ref(), a3.as_ref(), a4.as_ref()],
                    )
                }
                CmdVal::Quadruple(name, arg1, arg2, arg3) => {
                    a1 = format!(r#""{arg1}""#);
                    a2 = format!(r#""{arg2}""#);
                    a3 = format!(r#""{arg3}""#);
                    (name, vec![a1.as_ref(), a2.as_ref(), a3.as_ref()])
                }
                CmdVal::Tripple(name, arg1, arg2) => {
                    a1 = format!(r#""{arg1}""#);
                    a2 = format!(r#""{arg2}""#);
                    (name, vec![a1.as_ref(), a2.as_ref()])
                }
                CmdVal::Double(name, arg1) => {
                    a1 = format!(r#""{arg1}""#);
                    (name, vec![a1.as_ref()])
                }
                CmdVal::Single((name,)) => (name, vec![]),
            };
            if let Err(error) = mpv.command(&name.to_string(), &args) {
                eprintln!("failed to execute MPV command: '{error:#}'")
            }
        };

        fn set_property(name: impl ToString, value: impl SetData, mpv: &Mpv) {
            if let Err(error) = mpv.set_property(&name.to_string(), value) {
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
                    set_property(name, value, &mpv);
                }
                InMsg(InMsgFn::MpvCommand, InMsgArgs::Cmd(cmd)) => {
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
