use crate::stremio_app::ipc;
use crate::stremio_app::RPCResponse;
use flume::{Receiver, Sender};
use libmpv2::events::PropertyData;
use libmpv2::{events::Event, Format, Mpv, SetData};
use native_windows_gui::{self as nwg, PartialUi};
use once_cell::sync::Lazy;
use std::sync::Mutex;
use std::{
    mem, ptr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use winapi::shared::{
    minwindef::{DWORD, UINT},
    windef::{HMONITOR, HWND},
    winerror::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS},
};
use winapi::um::{
    wingdi::{
        DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER,
        DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME,
        QDC_ONLY_ACTIVE_PATHS,
    },
    winnt::LONG,
    winuser::{
        GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITORINFOEXW, MONITOR_DEFAULTTONEAREST,
    },
};

use crate::stremio_app::stremio_player::{
    CmdVal, InMsg, InMsgArgs, InMsgFn, MpvCmd, PlayerEnded, PlayerEvent, PlayerProprChange,
    PlayerResponse, PropKey, PropVal,
};

pub static CURRENT_TIME: Lazy<Mutex<f64>> = Lazy::new(|| Mutex::new(0.0));

pub static TOTAL_DURATION: Lazy<Mutex<f64>> = Lazy::new(|| Mutex::new(0.0));

pub static IS_PAUSED: Lazy<Mutex<bool>> = Lazy::new(|| Mutex::new(false));

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

#[link(name = "user32")]
extern "system" {
    fn GetDisplayConfigBufferSizes(
        flags: UINT,
        num_path_array_elements: *mut UINT,
        num_mode_info_array_elements: *mut UINT,
    ) -> LONG;
    fn QueryDisplayConfig(
        flags: UINT,
        num_path_array_elements: *mut UINT,
        path_array: *mut DISPLAYCONFIG_PATH_INFO,
        num_mode_info_array_elements: *mut UINT,
        mode_info_array: *mut DISPLAYCONFIG_MODE_INFO,
        current_topology_id: *mut u32,
    ) -> LONG;
    fn DisplayConfigGetDeviceInfo(request_packet: *mut DISPLAYCONFIG_DEVICE_INFO_HEADER) -> LONG;
}

const DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO_2: u32 = 15;
const DISPLAYCONFIG_ADVANCED_COLOR_MODE_HDR: u32 = 2;

#[repr(C)]
struct DisplayconfigGetAdvancedColorInfo2 {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    value: u32,
    color_encoding: u32,
    bits_per_color_channel: u32,
    active_color_mode: u32,
}

fn with_gpu_next_fallback(vo: String) -> String {
    let mut outputs = vo
        .split(',')
        .filter(|output| !output.is_empty())
        .map(String::from)
        .collect::<Vec<String>>();

    let has_gpu_next = outputs.iter().any(|output| output == "gpu-next");
    let has_gpu = outputs.iter().any(|output| output == "gpu");

    if outputs.is_empty() {
        outputs.push("gpu-next".to_string());
        outputs.push("gpu".to_string());
    } else if has_gpu_next && !has_gpu {
        outputs.push("gpu".to_string());
    } else if has_gpu && !has_gpu_next {
        outputs.push("gpu-next".to_string());
    }

    format!("{},", outputs.join(","))
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum DisplayOutputMode {
    Hdr,
    Sdr,
    Auto,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct DisplayOutputState {
    mode: DisplayOutputMode,
    scale_percent: u32,
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

        let mpv = Arc::new(create_mpv(window_handle));
        let mpv_event_client = mpv
            .create_client(None)
            .expect("cannot create MPV event client");

        let _event_thread = create_event_thread(
            mpv_event_client,
            observe_property_receiver,
            rpc_response_sender,
        );
        let gpu_video_processing = Arc::new(AtomicBool::new(false));
        let _display_thread = create_display_output_thread(
            Arc::clone(&mpv),
            window_handle as isize,
            Arc::clone(&gpu_video_processing),
        );
        let _message_thread = create_message_thread(
            mpv,
            window_handle as isize,
            gpu_video_processing,
            observe_property_sender,
            in_msg_receiver,
        );
        // @TODO implement a mechanism to stop threads on `Player` drop if needed

        Ok(())
    }
}

fn create_mpv(window_handle: HWND) -> Mpv {
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
        // gpu-next: libplacebo VO with modern HDR tone-mapping; gpu, is the fallback.
        set_property!("vo", "gpu-next,gpu,");
        for (name, value) in [
            ("gpu-context", "d3d11"),
            ("d3d11-output-format", "auto"),
            ("d3d11-output-csp", "auto"),
            ("target-colorspace-hint", "auto"),
            ("target-colorspace-hint-mode", "target"),
            ("tone-mapping", "bt.2390"),
            ("dither-depth", "auto"),
            ("deband", "yes"),
            ("scale", "spline36"),
            ("cscale", "spline36"),
        ] {
            if let Err(error) = initializer.set_property(name, value) {
                eprintln!("mpv: cannot set {name}={value}: {error:?}");
            }
        }
        Ok(())
    });
    mpv.expect("cannot build MPV")
}

fn create_display_output_thread(
    mpv: Arc<Mpv>,
    window_handle: isize,
    gpu_video_processing: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut last_state = None;

        loop {
            let state = current_display_output_state(&mpv, window_handle as HWND);
            let gpu = gpu_video_processing.load(Ordering::Relaxed);
            if last_state != Some((state, gpu)) {
                apply_display_output_mode(&mpv, state, gpu);
                last_state = Some((state, gpu));
            }
            thread::sleep(Duration::from_millis(500));
        }
    })
}

fn current_display_output_state(mpv: &Mpv, window_handle: HWND) -> DisplayOutputState {
    DisplayOutputState {
        mode: current_display_output_mode(window_handle),
        scale_percent: current_video_filter_scale(mpv, window_handle),
    }
}

fn current_display_output_mode(window_handle: HWND) -> DisplayOutputMode {
    let monitor = unsafe { MonitorFromWindow(window_handle, MONITOR_DEFAULTTONEAREST) };
    match monitor_hdr_active(monitor) {
        Some(true) => DisplayOutputMode::Hdr,
        Some(false) => DisplayOutputMode::Sdr,
        None => DisplayOutputMode::Auto,
    }
}

fn current_video_filter_scale(mpv: &Mpv, window_handle: HWND) -> u32 {
    let Some(video_height) = current_video_height(mpv) else {
        return 100;
    };
    let Some(display_height) = current_monitor_height(window_handle) else {
        return 100;
    };
    if video_height <= 0.0 || display_height <= video_height {
        return 100;
    }

    ((display_height / video_height).min(4.0) * 100.0).round() as u32
}

fn current_video_height(mpv: &Mpv) -> Option<f64> {
    let video_params = mpv.get_property::<String>("video-params").ok()?;
    let video_params = serde_json::from_str::<serde_json::Value>(&video_params).ok()?;
    video_params.get("h").and_then(serde_json::Value::as_f64)
}

fn current_monitor_height(window_handle: HWND) -> Option<f64> {
    let monitor = unsafe { MonitorFromWindow(window_handle, MONITOR_DEFAULTTONEAREST) };
    if monitor.is_null() {
        return None;
    }

    let mut monitor_info: MONITORINFO = unsafe { mem::zeroed() };
    monitor_info.cbSize = mem::size_of::<MONITORINFO>() as DWORD;
    if unsafe { GetMonitorInfoW(monitor, &mut monitor_info) } == 0 {
        return None;
    }

    Some((monitor_info.rcMonitor.bottom - monitor_info.rcMonitor.top) as f64)
}

fn apply_display_output_mode(mpv: &Mpv, state: DisplayOutputState, gpu_video_processing: bool) {
    // Target colorspace follows the display so native HDR still passes through; the RTX
    // HDR filter is the opt-in part and only makes sense on an HDR display.
    let vf = if gpu_video_processing {
        let scale = state.scale_percent as f64 / 100.0;
        let mut vf = format!("d3d11vpp=scaling-mode=nvidia:scale={scale:.2}");
        if state.mode == DisplayOutputMode::Hdr {
            vf.push_str(":format=x2bgr10:nvidia-true-hdr");
        }
        vf
    } else {
        String::new()
    };
    let color = match state.mode {
        DisplayOutputMode::Hdr | DisplayOutputMode::Auto => [
            ("d3d11-output-csp", "auto"),
            ("target-colorspace-hint", "auto"),
            ("target-trc", "auto"),
            ("target-prim", "auto"),
        ],
        DisplayOutputMode::Sdr => [
            ("d3d11-output-csp", "srgb"),
            ("target-colorspace-hint", "yes"),
            ("target-trc", "srgb"),
            ("target-prim", "bt.709"),
        ],
    };

    for (name, value) in std::iter::once(("vf", vf.as_str())).chain(color) {
        if let Err(error) = mpv.set_property(name, value) {
            eprintln!("mpv: cannot set {name}={value}: {error:?}");
        }
    }
}

fn monitor_hdr_active(monitor: HMONITOR) -> Option<bool> {
    if monitor.is_null() {
        return None;
    }

    let device_name = monitor_device_name(monitor)?;
    for path in active_display_paths()? {
        let Some(source_name) = display_source_name(&path) else {
            continue;
        };
        if source_name.viewGdiDeviceName != device_name {
            continue;
        }

        return display_hdr_active(&path);
    }

    None
}

fn monitor_device_name(monitor: HMONITOR) -> Option<[u16; 32]> {
    let mut monitor_info: MONITORINFOEXW = unsafe { mem::zeroed() };
    monitor_info.cbSize = mem::size_of::<MONITORINFOEXW>() as DWORD;

    let result =
        unsafe { GetMonitorInfoW(monitor, &mut monitor_info as *mut _ as *mut MONITORINFO) };
    if result == 0 {
        None
    } else {
        Some(monitor_info.szDevice)
    }
}

fn active_display_paths() -> Option<Vec<DISPLAYCONFIG_PATH_INFO>> {
    for _ in 0..3 {
        let mut path_count = 0;
        let mut mode_count = 0;
        let buffer_status = unsafe {
            GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count)
        };
        if buffer_status != ERROR_SUCCESS as LONG {
            return None;
        }

        let mut paths =
            vec![unsafe { mem::zeroed::<DISPLAYCONFIG_PATH_INFO>() }; path_count as usize];
        let mut modes =
            vec![unsafe { mem::zeroed::<DISPLAYCONFIG_MODE_INFO>() }; mode_count as usize];
        let query_status = unsafe {
            QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut path_count,
                paths.as_mut_ptr(),
                &mut mode_count,
                modes.as_mut_ptr(),
                ptr::null_mut(),
            )
        };

        if query_status == ERROR_SUCCESS as LONG {
            paths.truncate(path_count as usize);
            return Some(paths);
        }
        if query_status != ERROR_INSUFFICIENT_BUFFER as LONG {
            return None;
        }
    }

    None
}

fn display_source_name(path: &DISPLAYCONFIG_PATH_INFO) -> Option<DISPLAYCONFIG_SOURCE_DEVICE_NAME> {
    let mut source_name: DISPLAYCONFIG_SOURCE_DEVICE_NAME = unsafe { mem::zeroed() };
    source_name.header = DISPLAYCONFIG_DEVICE_INFO_HEADER {
        _type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
        size: mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
        adapterId: path.sourceInfo.adapterId,
        id: path.sourceInfo.id,
    };

    let status = unsafe { DisplayConfigGetDeviceInfo(&mut source_name.header) };
    if status == ERROR_SUCCESS as LONG {
        Some(source_name)
    } else {
        None
    }
}

fn display_hdr_active(path: &DISPLAYCONFIG_PATH_INFO) -> Option<bool> {
    let mut color_info: DisplayconfigGetAdvancedColorInfo2 = unsafe { mem::zeroed() };
    color_info.header = DISPLAYCONFIG_DEVICE_INFO_HEADER {
        _type: DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO_2,
        size: mem::size_of::<DisplayconfigGetAdvancedColorInfo2>() as u32,
        adapterId: path.targetInfo.adapterId,
        id: path.targetInfo.id,
    };

    let status = unsafe { DisplayConfigGetDeviceInfo(&mut color_info.header) };
    if status == ERROR_SUCCESS as LONG {
        Some(color_info.active_color_mode == DISPLAYCONFIG_ADVANCED_COLOR_MODE_HDR)
    } else {
        None
    }
}

fn create_event_thread(
    mut mpv_event_client: Mpv,
    observe_property_receiver: Receiver<ObserveProperty>,
    rpc_response_sender: Sender<String>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        mpv_event_client
            .disable_deprecated_events()
            .expect("failed to disable deprecated MPV events");

        loop {
            // Drain newly observed properties
            for ObserveProperty { name, format } in observe_property_receiver.drain() {
                mpv_event_client
                    .observe_property(&name, format, 0)
                    .expect("failed to observe MPV property");
            }

            let event = match mpv_event_client.wait_event(0.1) {
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
                Event::EndFile(reason) => PlayerResponse(
                    "mpv-event-ended",
                    PlayerEvent::End(PlayerEnded::from_end_reason(reason)),
                ),
                Event::Shutdown => {
                    break;
                }
                _ => continue,
            };

            if let Err(error) =
                rpc_response_sender.send(RPCResponse::response_message(player_response.to_value()))
            {
                eprintln!("failed to send RPCResponse: {error}");
                break;
            }
        }
    })
}

fn create_message_thread(
    mpv: Arc<Mpv>,
    window_handle: isize,
    gpu_video_processing: Arc<AtomicBool>,
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
            mpv.wake_up();
        }

        // -- Helpers --

        let observe_property = |name: String, format: Format| {
            if let Err(error) = observe_property_sender.send(ObserveProperty { name, format }) {
                eprintln!("cannot send ObserveProperty: {error}");
            }
        };

        let send_command = |cmd: CmdVal| {
            let parts: Vec<String> = cmd.into();
            if let Some((name, args)) = parts.split_first() {
                let args = args.iter().map(String::as_str).collect::<Vec<_>>();
                if let Err(error) = mpv.command(name, &args) {
                    eprintln!("failed to execute MPV command: '{error:#}'")
                }
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
                    let name_string = name.to_string();
                    let is_vo = name_string == "vo";
                    let value = if name_string == "sub-ass-override" && value == "strip" {
                        // Preserve ASS styling/positioning while still allowing subtitle scale changes.
                        "scale".to_string()
                    } else if is_vo {
                        with_gpu_next_fallback(value)
                    } else {
                        value
                    };
                    set_property(name, value, &mpv);
                    // vo reinit reverts color props to defaults; re-assert for the current display.
                    if is_vo {
                        apply_display_output_mode(
                            &mpv,
                            current_display_output_state(&mpv, window_handle as HWND),
                            gpu_video_processing.load(Ordering::Relaxed),
                        );
                    }
                }
                InMsg(InMsgFn::MpvSetGpuVideoProcessing, InMsgArgs::Flag(enabled)) => {
                    gpu_video_processing.store(enabled, Ordering::Relaxed);
                    apply_display_output_mode(
                        &mpv,
                        current_display_output_state(&mpv, window_handle as HWND),
                        enabled,
                    );
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
