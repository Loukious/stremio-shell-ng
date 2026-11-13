use crate::stremio_app::ipc;
use crate::stremio_app::mpv_hwnd::find_exact_mpv_child_hwnd;
use crate::stremio_app::shell_state;
use crate::stremio_app::stremio_wevbiew::wevbiew::set_bound_mpv_keys;
use crate::stremio_app::watch_party_local::{current_local_sync_revision, LocalSyncCommand};
use crate::stremio_app::RPCResponse;
use flume::{Receiver, Sender};
use libmpv2::events::PropertyData;
use libmpv2::{Format, Mpv};
use native_windows_gui::{self as nwg, PartialUi};
use once_cell::sync::Lazy;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use std::{mem, ptr};
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
        GetMonitorInfoW, GetWindow, IsWindow, MonitorFromWindow, SetWindowPos, GW_HWNDNEXT,
        HWND_BOTTOM, MONITORINFO, MONITORINFOEXW, MONITOR_DEFAULTTONEAREST, SWP_ASYNCWINDOWPOS,
        SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    },
};

use crate::stremio_app::stremio_player::{
    CmdVal, InMsg, InMsgArgs, InMsgFn, MpvCmd, PlayerEnded, PlayerEvent, PlayerProprChange,
    PlayerResponse, PropKey, PropVal,
};

pub static CURRENT_TIME: Lazy<Mutex<f64>> = Lazy::new(|| Mutex::new(0.0));

pub static TOTAL_DURATION: Lazy<Mutex<f64>> = Lazy::new(|| Mutex::new(0.0));

pub static IS_PAUSED: Lazy<Mutex<bool>> = Lazy::new(|| Mutex::new(false));

pub static CURRENT_VOLUME: Lazy<Mutex<f64>> = Lazy::new(|| Mutex::new(shell_state::DEFAULT_VOLUME));

pub static CACHED_PLAYER_PROPS: Lazy<Mutex<HashMap<String, Value>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

pub static IS_FILE_LOADED: Lazy<Mutex<bool>> = Lazy::new(|| Mutex::new(false));

/// Monotonically increases when a new file load is accepted, including reloads.
/// The epoch is retained through FileLoaded so consumers can reject old-file actions
/// throughout the transition as well as after playback begins.
pub static FILE_LOAD_EPOCH: AtomicU64 = AtomicU64::new(0);

pub static STOP_COMMAND_IN_FLIGHT: Lazy<Mutex<bool>> = Lazy::new(|| Mutex::new(false));

pub static MPV_VERSION: Lazy<Mutex<Option<String>>> = Lazy::new(|| Mutex::new(None));

pub static FFMPEG_VERSION: Lazy<Mutex<Option<String>>> = Lazy::new(|| Mutex::new(None));

/// Current chapter index as reported by mpv (or -1 if unknown / no chapters).
pub static CURRENT_CHAPTER: Lazy<Mutex<i64>> = Lazy::new(|| Mutex::new(-1));

/// Number of chapters in the current file.
pub static CURRENT_CHAPTER_COUNT: Lazy<Mutex<i64>> = Lazy::new(|| Mutex::new(0));

/// Current chapter title (from `chapter-metadata/by-key/title`).
pub static CURRENT_CHAPTER_TITLE: Lazy<Mutex<String>> = Lazy::new(|| Mutex::new(String::new()));

/// The actual video stream URL passed to `loadfile` (not the Stremio page URL).
pub static CURRENT_STREAM_URL: Lazy<Mutex<String>> = Lazy::new(|| Mutex::new(String::new()));

/// Global player command sender. Allows the sync client (and other subsystems)
/// to inject mpv commands without going through the webview pipeline.
/// Populated once during `on_init`.
pub static PLAYER_CMD_TX: Lazy<Mutex<Option<Sender<String>>>> = Lazy::new(|| Mutex::new(None));

static NEXT_MPV_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

const LOADFILE_START_TIMEOUT: Duration = Duration::from_secs(20);
const MPV_COMMAND_REPLY_TIMEOUT: Duration = Duration::from_secs(8);
const LOCAL_LOADFILE_WAIT_LOG_INTERVAL: Duration = Duration::from_secs(30);
const LOCAL_STREAM_STARTUP_RETRY_LIMIT: u32 = 60;
const LOCAL_STREAM_STARTUP_RETRY_DELAY: Duration = Duration::from_secs(5);
const MPV_COMMAND_QUEUE_RECOVERY_LIMIT: u32 = 3;
const DIRECT_HTTP_PREMATURE_END_RETRY_LIMIT: u32 = 3;
const DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO_2: u32 = 15;
const DISPLAYCONFIG_ADVANCED_COLOR_MODE_HDR: u32 = 2;

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

#[repr(C)]
struct DisplayconfigGetAdvancedColorInfo2 {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    value: u32,
    color_encoding: u32,
    bits_per_color_channel: u32,
    active_color_mode: u32,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoadState {
    Idle,
    Loading,
    Loaded,
    Stopping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrematureEndRecovery {
    Local,
    DirectHttp,
}

impl PrematureEndRecovery {
    fn retry_limit(self) -> u32 {
        match self {
            Self::Local => 1,
            Self::DirectHttp => DIRECT_HTTP_PREMATURE_END_RETRY_LIMIT,
        }
    }

    fn reload_url(self, url: &str) -> String {
        match self {
            Self::Local => cache_busted_url(url),
            // Debrid/CDN query strings may be signed. Reload them byte-for-byte.
            Self::DirectHttp => url.to_string(),
        }
    }
}

struct PlayerController {
    observed_properties: HashSet<String>,
    state: LoadState,
    file_epoch: u64,
    loaded_file_epoch: Option<u64>,
    load_epoch_reserved: bool,
    current_url: String,
    pending_loadfile: Option<CmdVal>,
    active_loadfile: Option<ActiveLoadfile>,
    pending_startup_props: Vec<(PropKey, PropVal)>,
    pending_startup_retry: Option<ScheduledStartupRetry>,
    pending_reload_url: Option<String>,
    pending_reload_seek: Option<f64>,
    startup_retry_attempts: u32,
    command_queue_recoveries: u32,
    premature_eof_reloads: u32,
    stop_started_at: Option<Instant>,
    stop_command_request_id: Option<u64>,
    persistent_render_properties: HashMap<String, PropVal>,
    mpv_child_hwnd: Option<HWND>,
    next_surface_order_refresh: Instant,
    window_handle: isize,
    gpu_video_processing: bool,
    managed_gpu_filter_applied: bool,
    last_display_output: Option<(DisplayOutputState, bool)>,
    next_display_refresh: Instant,
    video_ready: VideoReadyState,
    rpc_response_sender: Option<Sender<String>>,
    subtitle_request_sender: Option<Sender<SubtitleWorkerRequest>>,
    pending_media_command: Option<CmdVal>,
    pending_subtitle_ack: Option<Receiver<()>>,
}

struct ActiveLoadfile {
    cmd: CmdVal,
    url: String,
    command_request_id: u64,
    abortable: bool,
    queued_at: Instant,
    last_wait_log_at: Instant,
    command_replied: bool,
    start_file_seen: bool,
    file_loaded: bool,
    requested_start: Option<f64>,
}

struct ScheduledStartupRetry {
    due_at: Instant,
    expected_url: String,
    retry_url: String,
    attempt: u32,
    resume_position: Option<f64>,
}

#[derive(Clone, Copy)]
struct MpvClientHandle(*mut libmpv2_sys::mpv_handle);

#[derive(Clone, Copy)]
struct CommandSubmission {
    request_id: u64,
    abortable: bool,
}

#[derive(Debug, Eq, PartialEq)]
struct SubtitleCommand {
    name: &'static str,
    args: Vec<String>,
}

impl SubtitleCommand {
    fn from_command(command: CmdVal) -> Result<Self, CmdVal> {
        let name = match &command {
            CmdVal::Quintuple(MpvCmd::SubAdd, ..) => "sub-add",
            CmdVal::Double(MpvCmd::SubRemove, ..) => "sub-remove",
            _ => return Err(command),
        };
        Ok(Self {
            name,
            args: command.into(),
        })
    }
}

enum SubtitleWorkerRequest {
    Command(SubtitleCommand),
    BeginMediaChange(Sender<()>),
    EndMediaChange,
}

struct InFlightSubtitle {
    reply_id: u64,
    name: &'static str,
}

enum SubtitleWorkerEvent {
    Request(Result<SubtitleWorkerRequest, flume::RecvError>),
    Wake(Result<(), flume::RecvError>),
}

fn start_next_subtitle(
    mpv: &Mpv,
    queue: &mut VecDeque<SubtitleCommand>,
    in_flight: &mut Option<InFlightSubtitle>,
    next_reply_id: &mut u64,
    media_change: bool,
) {
    while !media_change && in_flight.is_none() {
        let Some(command) = queue.pop_front() else {
            return;
        };
        let args = match command
            .args
            .iter()
            .map(|argument| CString::new(argument.as_str()))
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(args) => args,
            Err(_) => {
                eprintln!("mpv: invalid {} argument", command.name);
                continue;
            }
        };
        let mut arg_ptrs = args
            .iter()
            .map(|arg| arg.as_ptr())
            .chain(std::iter::once(std::ptr::null::<c_char>()))
            .collect::<Vec<_>>();
        let reply_id = *next_reply_id;
        *next_reply_id = (*next_reply_id).wrapping_add(1).max(1);
        let result = unsafe {
            libmpv2_sys::mpv_command_async(mpv.ctx.as_ptr(), reply_id, arg_ptrs.as_mut_ptr())
        };
        if result < 0 {
            eprintln!(
                "mpv: failed to queue {}: {}",
                command.name,
                mpv_error_string(result)
            );
            continue;
        }
        *in_flight = Some(InFlightSubtitle {
            reply_id,
            name: command.name,
        });
    }
}

fn create_subtitle_thread(
    mut mpv: Mpv,
    request_receiver: Receiver<SubtitleWorkerRequest>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("subtitle-loader".to_string())
        .spawn(move || {
            let (wake_sender, wake_receiver) = flume::bounded(1);
            mpv.set_wakeup_callback(move || {
                let _ = wake_sender.try_send(());
            });

            let mut queue = VecDeque::new();
            let mut in_flight: Option<InFlightSubtitle> = None;
            let mut next_reply_id = 1;
            let mut media_change = false;
            let mut media_change_ack: Option<Sender<()>> = None;

            loop {
                let event = flume::Selector::new()
                    .recv(&request_receiver, SubtitleWorkerEvent::Request)
                    .recv(&wake_receiver, SubtitleWorkerEvent::Wake)
                    .wait();

                match event {
                    SubtitleWorkerEvent::Request(Ok(SubtitleWorkerRequest::Command(command))) => {
                        queue.push_back(command);
                    }
                    SubtitleWorkerEvent::Request(Ok(SubtitleWorkerRequest::BeginMediaChange(
                        acknowledge,
                    ))) => {
                        media_change = true;
                        queue.clear();
                        if let Some(command) = &in_flight {
                            media_change_ack = Some(acknowledge);
                            unsafe {
                                libmpv2_sys::mpv_abort_async_command(
                                    mpv.ctx.as_ptr(),
                                    command.reply_id,
                                );
                            }
                        } else {
                            let _ = acknowledge.send(());
                        }
                    }
                    SubtitleWorkerEvent::Request(Ok(SubtitleWorkerRequest::EndMediaChange)) => {
                        media_change = false;
                    }
                    SubtitleWorkerEvent::Request(Err(_)) | SubtitleWorkerEvent::Wake(Err(_)) => {
                        break;
                    }
                    SubtitleWorkerEvent::Wake(Ok(())) => loop {
                        let event = unsafe { &*libmpv2_sys::mpv_wait_event(mpv.ctx.as_ptr(), 0.0) };
                        match event.event_id {
                            libmpv2_sys::mpv_event_id_MPV_EVENT_NONE => break,
                            libmpv2_sys::mpv_event_id_MPV_EVENT_SHUTDOWN => return,
                            libmpv2_sys::mpv_event_id_MPV_EVENT_COMMAND_REPLY => {
                                let Some(command) = &in_flight else {
                                    continue;
                                };
                                if command.reply_id != event.reply_userdata {
                                    continue;
                                }
                                let command = in_flight.take().expect("missing subtitle command");
                                if event.error < 0 {
                                    eprintln!(
                                        "mpv: {} failed: {}",
                                        command.name,
                                        mpv_error_string(event.error)
                                    );
                                }
                                if let Some(acknowledge) = media_change_ack.take() {
                                    let _ = acknowledge.send(());
                                }
                            }
                            _ => {}
                        }
                    },
                }

                start_next_subtitle(
                    &mpv,
                    &mut queue,
                    &mut in_flight,
                    &mut next_reply_id,
                    media_change,
                );
            }
        })
        .expect("failed to start subtitle worker")
}

// The high-level event wrapper discards playlist entry IDs and command results.
// Copy the handled event data here so load readiness can use MPV's actual IDs.
#[derive(Debug)]
enum PlayerMpvEvent {
    GetPropertyReply(String, OwnedPropertyData),
    SetPropertyReply,
    PropertyChange(String, OwnedPropertyData),
    CommandReply {
        request_id: u64,
        error: i32,
        playlist_entry_id: Option<i64>,
    },
    StartFile(i64),
    FileLoaded,
    PlaybackRestart,
    EndFile(libmpv2::EndFileReason),
    Shutdown,
    Other,
}

#[derive(Debug)]
enum OwnedPropertyData {
    Str(String),
    OsdStr(String),
    Flag(bool),
    Int64(i64),
    Double(f64),
}

impl OwnedPropertyData {
    fn as_property_data(&self) -> PropertyData<'_> {
        match self {
            Self::Str(value) => PropertyData::Str(value),
            Self::OsdStr(value) => PropertyData::OsdStr(value),
            Self::Flag(value) => PropertyData::Flag(*value),
            Self::Int64(value) => PropertyData::Int64(*value),
            Self::Double(value) => PropertyData::Double(*value),
        }
    }
}

fn wait_player_event(mpv: &Mpv, timeout: f64) -> Option<Result<PlayerMpvEvent, String>> {
    let event = unsafe { &*libmpv2_sys::mpv_wait_event(mpv.ctx.as_ptr(), timeout) };
    use libmpv2_sys::{
        mpv_event_command, mpv_event_end_file, mpv_event_property, mpv_event_start_file,
    };
    if event.event_id == libmpv2_sys::mpv_event_id_MPV_EVENT_NONE {
        return None;
    }
    if event.event_id == libmpv2_sys::mpv_event_id_MPV_EVENT_COMMAND_REPLY {
        let playlist_entry_id = if event.error >= 0 && !event.data.is_null() {
            let command = unsafe { &*(event.data as *const mpv_event_command) };
            playlist_entry_id_from_node(&command.result)
        } else {
            None
        };
        return Some(Ok(PlayerMpvEvent::CommandReply {
            request_id: event.reply_userdata,
            error: event.error,
            playlist_entry_id,
        }));
    }
    if event.error < 0 {
        return Some(Err(self::mpv_error_string(event.error)));
    }
    let result = match event.event_id {
        libmpv2_sys::mpv_event_id_MPV_EVENT_START_FILE if !event.data.is_null() => {
            let start = unsafe { &*(event.data as *const mpv_event_start_file) };
            Ok(PlayerMpvEvent::StartFile(start.playlist_entry_id))
        }
        libmpv2_sys::mpv_event_id_MPV_EVENT_FILE_LOADED => Ok(PlayerMpvEvent::FileLoaded),
        libmpv2_sys::mpv_event_id_MPV_EVENT_PLAYBACK_RESTART => Ok(PlayerMpvEvent::PlaybackRestart),
        libmpv2_sys::mpv_event_id_MPV_EVENT_SHUTDOWN => Ok(PlayerMpvEvent::Shutdown),
        libmpv2_sys::mpv_event_id_MPV_EVENT_END_FILE if !event.data.is_null() => {
            let end = unsafe { &*(event.data as *const mpv_event_end_file) };
            if end.error < 0 {
                eprintln!("[MPV] EndFile error: {}", self::mpv_error_string(end.error));
            }
            // The reason still needs the normal EndFile handling, including recovery
            // and notifying the frontend when playback cannot continue.
            Ok(PlayerMpvEvent::EndFile(end.reason))
        }
        libmpv2_sys::mpv_event_id_MPV_EVENT_GET_PROPERTY_REPLY
        | libmpv2_sys::mpv_event_id_MPV_EVENT_PROPERTY_CHANGE
            if !event.data.is_null() =>
        {
            let property = unsafe { &*(event.data as *const mpv_event_property) };
            if property.format == libmpv2_sys::mpv_format_MPV_FORMAT_NONE {
                // Unload notifications can precede EndFile. Keep the last valid
                // playback position, duration, and seekability for recovery.
                return None;
            }
            if property.name.is_null() || property.data.is_null() {
                return Some(Err("MPV returned an empty property event".to_owned()));
            }
            let name = unsafe { CStr::from_ptr(property.name) }
                .to_string_lossy()
                .into_owned();
            let value = unsafe {
                match property.format {
                    libmpv2_sys::mpv_format_MPV_FORMAT_FLAG => {
                        OwnedPropertyData::Flag(*(property.data as *const i32) != 0)
                    }
                    libmpv2_sys::mpv_format_MPV_FORMAT_INT64 => {
                        OwnedPropertyData::Int64(*(property.data as *const i64))
                    }
                    libmpv2_sys::mpv_format_MPV_FORMAT_DOUBLE => {
                        OwnedPropertyData::Double(*(property.data as *const f64))
                    }
                    libmpv2_sys::mpv_format_MPV_FORMAT_STRING
                    | libmpv2_sys::mpv_format_MPV_FORMAT_OSD_STRING => {
                        let text = *(property.data as *const *const c_char);
                        if text.is_null() {
                            return Some(Err("MPV returned an empty property string".to_owned()));
                        }
                        let text = CStr::from_ptr(text).to_string_lossy().into_owned();
                        if property.format == libmpv2_sys::mpv_format_MPV_FORMAT_STRING {
                            OwnedPropertyData::Str(text)
                        } else {
                            OwnedPropertyData::OsdStr(text)
                        }
                    }
                    _ => {
                        return Some(Err("MPV returned an unsupported property format".to_owned()))
                    }
                }
            };
            if event.event_id == libmpv2_sys::mpv_event_id_MPV_EVENT_GET_PROPERTY_REPLY {
                Ok(PlayerMpvEvent::GetPropertyReply(name, value))
            } else {
                Ok(PlayerMpvEvent::PropertyChange(name, value))
            }
        }
        libmpv2_sys::mpv_event_id_MPV_EVENT_SET_PROPERTY_REPLY => {
            Ok(PlayerMpvEvent::SetPropertyReply)
        }
        _ => Ok(PlayerMpvEvent::Other),
    };
    Some(result)
}

fn playlist_entry_id_from_node(node: &libmpv2_sys::mpv_node) -> Option<i64> {
    if node.format != libmpv2_sys::mpv_format_MPV_FORMAT_NODE_MAP {
        return None;
    }
    let list = unsafe { node.u.list.as_ref()? };
    if list.keys.is_null() || list.values.is_null() {
        return None;
    }
    for index in 0..list.num.max(0) as usize {
        let key = unsafe { *list.keys.add(index) };
        let value = unsafe { &*list.values.add(index) };
        if !key.is_null()
            && unsafe { CStr::from_ptr(key) }.to_bytes() == b"playlist_entry_id"
            && value.format == libmpv2_sys::mpv_format_MPV_FORMAT_INT64
        {
            return Some(unsafe { value.u.int64 });
        }
    }
    None
}

#[derive(Debug, Default)]
struct VideoReadyState {
    next_load_id: u64,
    current_load_id: Option<u64>,
    request_load_ids: HashMap<u64, u64>,
    playlist_load_ids: HashMap<i64, u64>,
    active_playlist_entry_id: Option<i64>,
    active_load_id: Option<u64>,
    file_loaded_id: Option<u64>,
    file_loaded: bool,
    ready_sent_id: Option<u64>,
}

impl VideoReadyState {
    fn begin_transition(&mut self) -> u64 {
        self.next_load_id += 1;
        self.current_load_id = Some(self.next_load_id);
        self.file_loaded_id = None;
        self.file_loaded = false;
        self.ready_sent_id = None;
        self.next_load_id
    }

    fn queue_current_load(&mut self, request_id: u64) {
        if let Some(load_id) = self.current_load_id {
            self.request_load_ids.insert(request_id, load_id);
        }
    }

    fn command_replied(&mut self, request_id: u64, playlist_entry_id: Option<i64>) {
        let Some(load_id) = self.request_load_ids.remove(&request_id) else {
            return;
        };
        if let Some(entry_id) = playlist_entry_id {
            if self.active_playlist_entry_id == Some(entry_id) {
                self.active_load_id = Some(load_id);
                if self.file_loaded && Some(load_id) == self.current_load_id {
                    self.file_loaded_id = Some(load_id);
                }
            } else {
                self.playlist_load_ids.insert(entry_id, load_id);
            }
        }
    }

    fn end_file(&mut self) {
        self.file_loaded_id = None;
        self.file_loaded = false;
        self.ready_sent_id = None;
    }

    fn start_file(&mut self, playlist_entry_id: i64) {
        self.active_playlist_entry_id = Some(playlist_entry_id);
        self.active_load_id = self.playlist_load_ids.remove(&playlist_entry_id);
        self.file_loaded_id = None;
        self.file_loaded = false;
    }

    fn file_loaded(&mut self) {
        self.file_loaded = true;
        if self.active_load_id == self.current_load_id {
            self.file_loaded_id = self.active_load_id;
        }
    }

    fn playback_restarted(&mut self) -> Option<u64> {
        let load_id = self.current_load_id?;
        if self.file_loaded_id != Some(load_id) || self.ready_sent_id == Some(load_id) {
            return None;
        }
        self.ready_sent_id = Some(load_id);
        Some(load_id)
    }
}

fn video_ready_response(load_id: u64, ready: bool) -> String {
    RPCResponse::response_message(PlayerResponse::video_ready(load_id, ready).to_value())
}

fn send_video_ready(rpc_response_sender: &Sender<String>, load_id: u64, ready: bool) {
    if let Err(error) = rpc_response_sender.send(video_ready_response(load_id, ready)) {
        eprintln!("failed to send video readiness: {error}");
    }
}

impl PlayerController {
    fn new(window_handle: isize) -> Self {
        Self {
            observed_properties: HashSet::new(),
            state: LoadState::Idle,
            file_epoch: 0,
            loaded_file_epoch: None,
            load_epoch_reserved: false,
            current_url: String::new(),
            pending_loadfile: None,
            active_loadfile: None,
            pending_startup_props: Vec::new(),
            pending_startup_retry: None,
            pending_reload_url: None,
            pending_reload_seek: None,
            startup_retry_attempts: 0,
            command_queue_recoveries: 0,
            premature_eof_reloads: 0,
            stop_started_at: None,
            stop_command_request_id: None,
            persistent_render_properties: HashMap::new(),
            mpv_child_hwnd: None,
            next_surface_order_refresh: Instant::now(),
            window_handle,
            gpu_video_processing: false,
            managed_gpu_filter_applied: false,
            last_display_output: None,
            next_display_refresh: Instant::now(),
            video_ready: VideoReadyState::default(),
            rpc_response_sender: None,
            subtitle_request_sender: None,
            pending_media_command: None,
            pending_subtitle_ack: None,
        }
    }

    fn refresh_display_output(&mut self, mpv: &Mpv) {
        if self.state != LoadState::Loaded {
            return;
        }

        let now = Instant::now();
        if now < self.next_display_refresh {
            return;
        }
        self.next_display_refresh = now + Duration::from_millis(500);

        let state =
            current_display_output_state(self.window_handle as HWND, self.gpu_video_processing);
        let key = (state, self.gpu_video_processing);
        if self.last_display_output != Some(key) {
            self.managed_gpu_filter_applied = apply_display_output_mode(
                mpv,
                state,
                self.gpu_video_processing,
                self.managed_gpu_filter_applied,
            );
            self.last_display_output = Some(key);
        }
    }

    fn invalidate_display_output(&mut self) {
        self.last_display_output = None;
        self.next_display_refresh = Instant::now();
    }

    fn keep_video_surface_behind_webview(&mut self) {
        let now = Instant::now();
        if now < self.next_surface_order_refresh {
            return;
        }
        self.next_surface_order_refresh = now + Duration::from_millis(250);

        let child = self
            .mpv_child_hwnd
            .filter(|hwnd| unsafe { IsWindow(*hwnd) } != 0)
            .or_else(|| find_exact_mpv_child_hwnd(self.window_handle as HWND));
        let Some(child) = child else {
            self.mpv_child_hwnd = None;
            return;
        };
        self.mpv_child_hwnd = Some(child);

        // The WebView2 controller and mpv are sibling windows. Keep mpv at the
        // bottom so the transparent player area can reveal video without the
        // force-window idle frame covering the rest of the application.
        if unsafe { GetWindow(child, GW_HWNDNEXT) }.is_null() {
            return;
        }
        unsafe {
            SetWindowPos(
                child,
                HWND_BOTTOM,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS,
            );
        }
    }
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
        data.channel = ipc::Channel::new(Some((in_msg_sender, rpc_response_receiver)));

        let mpv = create_shareable_mpv(window_handle);
        // Player is built before WebView. Discover bindings here so the first document gets a
        // complete key set instead of racing the event thread during WebView initialization.
        query_and_set_bound_keys(mpv.ctx.as_ptr());
        let _event_thread = create_event_thread(
            mpv,
            window_handle as isize,
            in_msg_receiver,
            rpc_response_sender,
        );
        // @TODO implement a mechanism to stop threads on `Player` drop if needed

        Ok(())
    }
}

fn create_shareable_mpv(window_handle: HWND) -> Mpv {
    let initial_volume = shell_state::load_volume();
    *CURRENT_VOLUME.lock().unwrap() = initial_volume;

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
        set_property!("audio-fallback-to-null", "yes");
        set_property!("config", "yes");
        set_property!("load-scripts", "yes");
        // Keep the embedded VO alive between files. Recreating it at every EOF can
        // deadlock inside a GPU driver's presentation teardown before the next
        // loadfile command reaches mpv.
        set_property!("idle", "yes");
        set_property!("force-window", "immediate");
        set_property!("terminal", "yes");
        #[cfg(debug_assertions)]
        set_property!("msg-level", "all=no,cplayer=debug");
        #[cfg(not(debug_assertions))]
        set_property!("msg-level", "all=no");
        set_property!("quiet", "yes");
        set_property!("volume", initial_volume);
        set_property!("osd-bar-marker-style", "none");
        set_property!("hwdec", "auto");
        // `%23%` escapes the 23-byte HTTP status list as one mpv option value.
        set_property!(
            "stream-lavf-o",
            "reconnect=1,reconnect_on_network_error=1,reconnect_on_http_error=%23%408,429,500,502,503,504,reconnect_delay_max=15"
        );
        // gpu-next: libplacebo VO with modern HDR tone-mapping; gpu, is the fallback.
        set_property!("vo", "gpu-next,gpu,");
        for (name, value) in [
            // Let mpv.conf choose a backend. On Windows, auto still selects the
            // appropriate native context when the user has not configured one.
            ("gpu-context", "auto"),
            ("d3d11-output-format", "auto"),
            ("d3d11-output-csp", "auto"),
            ("target-colorspace-hint", "auto"),
            ("target-colorspace-hint-mode", "target"),
            ("tone-mapping", "bt.2390"),
            ("dither-depth", "auto"),
            ("deband", "yes"),
        ] {
            if let Err(error) = initializer.set_property(name, value) {
                eprintln!("mpv: cannot set {name}={value}: {error:?}");
            }
        }
        Ok(())
    });
    mpv.expect("cannot build MPV")
}

fn with_gpu_next_fallback(vo: String) -> String {
    let mut outputs = vo
        .split(',')
        .filter(|output| !output.is_empty())
        .map(String::from)
        .collect::<Vec<_>>();
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

fn current_display_output_state(
    window_handle: HWND,
    gpu_video_processing: bool,
) -> DisplayOutputState {
    DisplayOutputState {
        mode: current_display_output_mode(window_handle),
        scale_percent: if gpu_video_processing {
            current_video_filter_scale(window_handle)
        } else {
            100
        },
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

fn current_video_filter_scale(window_handle: HWND) -> u32 {
    let Some(video_height) = current_video_height() else {
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

fn current_video_height() -> Option<f64> {
    let video_params = CACHED_PLAYER_PROPS
        .lock()
        .unwrap()
        .get("video-params")
        .cloned()?;
    let video_params = match video_params {
        Value::String(value) => serde_json::from_str::<Value>(&value).ok()?,
        value => value,
    };
    video_params.get("h").and_then(Value::as_f64)
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

fn apply_display_output_mode(
    mpv: &Mpv,
    state: DisplayOutputState,
    gpu_video_processing: bool,
    managed_filter_applied: bool,
) -> bool {
    let gpu_filter = if gpu_video_processing {
        let scale = state.scale_percent as f64 / 100.0;
        let mut vf = format!("d3d11vpp=scaling-mode=nvidia:scale={scale:.2}");
        if state.mode == DisplayOutputMode::Hdr {
            vf.push_str(":format=x2bgr10:nvidia-true-hdr");
        }
        Some(format!("@stremio-gpu-processing:{vf}"))
    } else {
        None
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
            ("target-trc", "auto"),
            ("target-prim", "bt.709"),
        ],
    };

    let mut filter_applied = managed_filter_applied;
    if managed_filter_applied
        && send_command_parts(
            mpv,
            "vf".to_string(),
            vec!["remove".to_string(), "@stremio-gpu-processing".to_string()],
        )
        .is_some()
    {
        filter_applied = false;
    }

    if let Some(filter) = gpu_filter {
        if send_command_parts(mpv, "vf".to_string(), vec!["add".to_string(), filter]).is_some() {
            filter_applied = true;
        }
    }

    for (name, value) in color {
        let _ = set_property_async(name, &PropVal::Str(value.to_string()), mpv);
    }

    filter_applied
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
        if source_name.viewGdiDeviceName == device_name {
            return display_hdr_active(&path);
        }
    }

    None
}

fn monitor_device_name(monitor: HMONITOR) -> Option<[u16; 32]> {
    let mut monitor_info: MONITORINFOEXW = unsafe { mem::zeroed() };
    monitor_info.cbSize = mem::size_of::<MONITORINFOEXW>() as DWORD;
    let result =
        unsafe { GetMonitorInfoW(monitor, &mut monitor_info as *mut _ as *mut MONITORINFO) };
    (result != 0).then_some(monitor_info.szDevice)
}

fn active_display_paths() -> Option<Vec<DISPLAYCONFIG_PATH_INFO>> {
    for _ in 0..3 {
        let mut path_count = 0;
        let mut mode_count = 0;
        let status = unsafe {
            GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count)
        };
        if status != ERROR_SUCCESS as LONG {
            return None;
        }

        let mut paths =
            vec![unsafe { mem::zeroed::<DISPLAYCONFIG_PATH_INFO>() }; path_count as usize];
        let mut modes =
            vec![unsafe { mem::zeroed::<DISPLAYCONFIG_MODE_INFO>() }; mode_count as usize];
        let status = unsafe {
            QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut path_count,
                paths.as_mut_ptr(),
                &mut mode_count,
                modes.as_mut_ptr(),
                ptr::null_mut(),
            )
        };

        if status == ERROR_SUCCESS as LONG {
            paths.truncate(path_count as usize);
            return Some(paths);
        }
        if status != ERROR_INSUFFICIENT_BUFFER as LONG {
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
    (status == ERROR_SUCCESS as LONG).then_some(source_name)
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
    (status == ERROR_SUCCESS as LONG)
        .then_some(color_info.active_color_mode == DISPLAYCONFIG_ADVANCED_COLOR_MODE_HDR)
}

fn create_event_thread(
    mpv: Mpv,
    window_handle: isize,
    in_msg_receiver: Receiver<String>,
    rpc_response_sender: Sender<String>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut controller = PlayerController::new(window_handle);
        controller.rpc_response_sender = Some(rpc_response_sender.clone());
        let subtitle_mpv = mpv
            .create_client(None)
            .expect("cannot create MPV subtitle client");
        let (subtitle_sender, subtitle_receiver) = flume::unbounded();
        controller.subtitle_request_sender = Some(subtitle_sender);
        let _subtitle_thread = create_subtitle_thread(subtitle_mpv, subtitle_receiver);
        mpv.disable_deprecated_events()
            .expect("failed to disable deprecated MPV events");
        for (name, format) in [
            ("time-pos", Format::Double),
            ("duration", Format::Double),
            ("seekable", Format::Flag),
            ("pause", Format::Flag),
            ("chapter", Format::Int64),
            ("chapters", Format::Int64),
            ("chapter-metadata/by-key/title", Format::String),
            ("audio-device-list", Format::String),
        ] {
            observe_property(&mpv, name, format);
            controller.observed_properties.insert(name.to_string());
        }

        loop {
            while let Ok(msg) = in_msg_receiver.try_recv() {
                controller.handle_in_msg(&mpv, msg, &rpc_response_sender);
            }
            controller.flush_pending_media_command(&mpv);
            controller.flush_stale_stop(&mpv);
            controller.flush_stale_loadfile(&mpv);
            controller.flush_pending_startup_retry(&mpv);
            controller.refresh_display_output(&mpv);
            controller.keep_video_surface_behind_webview();

            let event = match wait_player_event(&mpv, 0.01) {
                Some(Ok(event)) => event,
                Some(Err(error)) => {
                    eprintln!("Event errored: {error:?}");
                    let resume_position = controller
                        .active_loadfile
                        .as_ref()
                        .and_then(|active| active.requested_start);
                    controller.retry_local_stream_startup_failure("event error", resume_position);
                    continue;
                }
                None => continue,
            };

            let player_response = match event {
                PlayerMpvEvent::GetPropertyReply(name, result) => {
                    let result = result.as_property_data();
                    if name == "current-ao" {
                        if matches!(
                            result,
                            PropertyData::Str("null") | PropertyData::OsdStr("null")
                        ) {
                            let _ = send_command_parts(&mpv, "ao-reload".to_string(), Vec::new());
                        }
                        continue;
                    }
                    emit_async_property_reply(&rpc_response_sender, &name, &result);
                    continue;
                }
                PlayerMpvEvent::SetPropertyReply => continue,
                PlayerMpvEvent::StartFile(playlist_entry_id) => {
                    println!("[MPV] StartFile");
                    controller.on_start_file(playlist_entry_id);
                    continue;
                }
                PlayerMpvEvent::FileLoaded => {
                    let url = controller.current_url.clone();
                    println!("[MPV] FileLoaded url={url}");
                    if !controller.on_file_loaded(&mpv, &rpc_response_sender) {
                        continue;
                    }
                    request_loaded_property_snapshot(&mpv);

                    if let Some(pos) = controller.take_pending_reload_seek_for(&url) {
                        println!("[MPV] Restoring position after reload: {}", pos);
                        let _ = send_command(
                            &mpv,
                            CmdVal::Tripple(MpvCmd::Seek, pos.to_string(), "absolute".to_string()),
                        );
                    }
                    continue;
                }
                PlayerMpvEvent::PlaybackRestart => {
                    if let Some(load_id) = controller.video_ready.playback_restarted() {
                        send_video_ready(&rpc_response_sender, load_id, true);
                    }
                    continue;
                }
                PlayerMpvEvent::PropertyChange(name, change) => {
                    let change = change.as_property_data();
                    if name == "audio-device-list" {
                        request_property_async(&mpv, "current-ao", Format::String);
                        continue;
                    }
                    update_cached_property(&name, &change);
                    PlayerResponse(
                        "mpv-prop-change",
                        PlayerEvent::PropChange(PlayerProprChange::from_name_value(
                            name.to_string(),
                            change,
                        )),
                    )
                }
                PlayerMpvEvent::EndFile(reason) => {
                    let url = controller.current_url.clone();
                    let time = *CURRENT_TIME.lock().unwrap();
                    let duration = *TOTAL_DURATION.lock().unwrap();
                    let replacement_queued = controller.pending_media_command.is_some()
                        || controller.pending_loadfile.is_some();
                    let recovery = premature_end_recovery(
                        controller.state,
                        reason,
                        &url,
                        time,
                        duration,
                        cached_property_value("seekable").and_then(|value| value.as_bool())
                            == Some(true),
                    )
                    .filter(|_| !replacement_queued);
                    println!(
                        "[MPV] EndFile reason={:?}, time={}, duration={}, url={}",
                        reason, time, duration, url
                    );

                    let error_retry_scheduled = if reason == libmpv2::mpv_end_file_reason::Error {
                        let resume_position = controller
                            .active_loadfile
                            .as_ref()
                            .and_then(|active| active.requested_start);
                        controller
                            .retry_local_stream_startup_failure("end-file error", resume_position)
                    } else {
                        false
                    };
                    controller.on_end_file();
                    if controller.send_pending_loadfile(&mpv) {
                        continue;
                    }

                    if recovery.is_some_and(|recovery| {
                        controller.recover_premature_end(&mpv, recovery, &url, time)
                    }) {
                        // Suppress completion only after a replacement was actually queued.
                        continue;
                    }

                    if error_retry_scheduled {
                        continue;
                    }

                    if matches!(
                        reason,
                        libmpv2::mpv_end_file_reason::Stop
                            | libmpv2::mpv_end_file_reason::Quit
                            | libmpv2::mpv_end_file_reason::Redirect
                    ) {
                        println!("[MPV] Suppressing non-terminal EndFile reason={:?}", reason);
                        continue;
                    }

                    PlayerResponse(
                        "mpv-event-ended",
                        PlayerEvent::End(PlayerEnded::from_end_reason(unrecovered_end_reason(
                            reason, recovery,
                        ))),
                    )
                }
                PlayerMpvEvent::Shutdown => {
                    crate::stremio_app::steam_sync::notify_player_unloaded(controller.file_epoch);
                    break;
                }
                PlayerMpvEvent::CommandReply {
                    request_id,
                    error,
                    playlist_entry_id,
                } => {
                    controller
                        .video_ready
                        .command_replied(request_id, playlist_entry_id);
                    if error < 0 {
                        eprintln!(
                            "MPV command {request_id} failed: {}",
                            mpv_error_string(error)
                        );
                        let resume_position = controller
                            .active_loadfile
                            .as_ref()
                            .and_then(|active| active.requested_start);
                        controller.retry_local_stream_startup_failure(
                            "command reply error",
                            resume_position,
                        );
                        continue;
                    }
                    println!("[MPV CMD REPLY] request_id={request_id}");
                    controller.on_command_reply(request_id, &mpv);
                    continue;
                }
                _ => continue,
            };

            rpc_response_sender
                .send(RPCResponse::response_message(player_response.to_value()))
                .expect("failed to send RPCResponse");
        }
    })
}

fn run_mpv_command_async(
    command_handle: MpvClientHandle,
    request_id: u64,
    name: &str,
    args: &[String],
) -> std::result::Result<(), String> {
    let mut cstr_args = Vec::with_capacity(args.len() + 1);
    cstr_args.push(CString::new(name).map_err(|error| error.to_string())?);
    for arg in args {
        cstr_args.push(CString::new(arg.as_str()).map_err(|error| error.to_string())?);
    }

    let mut ptrs = cstr_args
        .iter()
        .map(|arg| arg.as_ptr())
        .collect::<Vec<*const c_char>>();
    ptrs.push(std::ptr::null());

    let result =
        unsafe { libmpv2_sys::mpv_command_async(command_handle.0, request_id, ptrs.as_mut_ptr()) };
    if result < 0 {
        Err(mpv_error_string(result))
    } else {
        Ok(())
    }
}

fn run_mpv_set_property_async(
    command_handle: MpvClientHandle,
    request_id: u64,
    name: &str,
    value: &PropVal,
) -> std::result::Result<(), String> {
    let name = CString::new(name).map_err(|error| error.to_string())?;
    let result = match value {
        PropVal::Bool(value) => {
            let mut data = i64::from(*value);
            unsafe {
                libmpv2_sys::mpv_set_property_async(
                    command_handle.0,
                    request_id,
                    name.as_ptr(),
                    libmpv2_sys::mpv_format_MPV_FORMAT_FLAG,
                    (&mut data as *mut i64).cast(),
                )
            }
        }
        PropVal::Num(value) => {
            let mut data = *value;
            unsafe {
                libmpv2_sys::mpv_set_property_async(
                    command_handle.0,
                    request_id,
                    name.as_ptr(),
                    libmpv2_sys::mpv_format_MPV_FORMAT_DOUBLE,
                    (&mut data as *mut f64).cast(),
                )
            }
        }
        PropVal::Str(value) => {
            let value = CString::new(value.as_str()).map_err(|error| error.to_string())?;
            let mut data = value.as_ptr();
            unsafe {
                libmpv2_sys::mpv_set_property_async(
                    command_handle.0,
                    request_id,
                    name.as_ptr(),
                    libmpv2_sys::mpv_format_MPV_FORMAT_STRING,
                    (&mut data as *mut *const c_char).cast(),
                )
            }
        }
    };

    if result < 0 {
        Err(mpv_error_string(result))
    } else {
        Ok(())
    }
}

fn run_mpv_get_property_async(
    command_handle: MpvClientHandle,
    request_id: u64,
    name: &str,
    format: Format,
) -> std::result::Result<(), String> {
    let name = CString::new(name).map_err(|error| error.to_string())?;
    let format = match format {
        Format::String => libmpv2_sys::mpv_format_MPV_FORMAT_STRING,
        Format::Flag => libmpv2_sys::mpv_format_MPV_FORMAT_FLAG,
        Format::Int64 => libmpv2_sys::mpv_format_MPV_FORMAT_INT64,
        Format::Double => libmpv2_sys::mpv_format_MPV_FORMAT_DOUBLE,
        Format::Node => return Err("async node property reads are unsupported".to_string()),
    };
    let result = unsafe {
        libmpv2_sys::mpv_get_property_async(command_handle.0, request_id, name.as_ptr(), format)
    };

    if result < 0 {
        Err(mpv_error_string(result))
    } else {
        Ok(())
    }
}

/// Query mpv's `input-bindings` property and populate the webview's
/// bound-key set with every non-weak (user-defined) binding key.
///
/// `input-bindings` returns a `NODE_ARRAY` of `NODE_MAP` entries.
/// Each map has at least:
///   - "key"     (STRING)  – the mpv key name
///   - "is_weak" (FLAG)    – true for built-in/default bindings
///   - "priority" (INT64)  – negative for inactive bindings
fn query_and_set_bound_keys(ctx: *mut libmpv2_sys::mpv_handle) {
    use std::ffi::CStr;

    let name = match CString::new("input-bindings") {
        Ok(n) => n,
        Err(_) => return,
    };

    let mut node: libmpv2_sys::mpv_node = unsafe { mem::zeroed() };
    let rc = unsafe {
        libmpv2_sys::mpv_get_property(
            ctx,
            name.as_ptr(),
            libmpv2_sys::mpv_format_MPV_FORMAT_NODE,
            (&mut node as *mut libmpv2_sys::mpv_node).cast(),
        )
    };

    if rc < 0 {
        eprintln!(
            "[MPV KEYS] failed to get input-bindings: {}",
            mpv_error_string(rc)
        );
        return;
    }

    let mut keys = HashSet::new();
    let mut has_sequences = false;

    // The top-level node must be an array.
    if node.format == libmpv2_sys::mpv_format_MPV_FORMAT_NODE_ARRAY {
        let list_ptr = unsafe { node.u.list };
        if !list_ptr.is_null() {
            let list = unsafe { &*list_ptr };
            for i in 0..list.num as usize {
                let entry = unsafe { &*list.values.add(i) };
                // Each entry should be a map.
                if entry.format != libmpv2_sys::mpv_format_MPV_FORMAT_NODE_MAP {
                    continue;
                }
                let map_ptr = unsafe { entry.u.list };
                if map_ptr.is_null() {
                    continue;
                }
                let map = unsafe { &*map_ptr };

                let mut key_name = None;
                let mut is_weak = true;
                let mut priority = -1_i64;

                for j in 0..map.num as usize {
                    let field_key =
                        unsafe { CStr::from_ptr(*map.keys.add(j)).to_str().unwrap_or("") };
                    let field_val = unsafe { &*map.values.add(j) };

                    match field_key {
                        "key" if field_val.format == libmpv2_sys::mpv_format_MPV_FORMAT_STRING => {
                            key_name = unsafe { CStr::from_ptr(field_val.u.string).to_str().ok() }
                                .map(str::to_owned);
                        }
                        "is_weak"
                            if field_val.format == libmpv2_sys::mpv_format_MPV_FORMAT_FLAG =>
                        {
                            is_weak = unsafe { field_val.u.flag } != 0;
                        }
                        "priority"
                            if field_val.format == libmpv2_sys::mpv_format_MPV_FORMAT_INT64 =>
                        {
                            priority = unsafe { field_val.u.int64 };
                        }
                        _ => {}
                    }
                }

                // Negative-priority bindings are inactive. Weak bindings are defaults that the
                // Web UI should continue to own unless the user overrides them.
                if is_active_user_binding(is_weak, priority) {
                    if let Some(key) = key_name {
                        let parts = binding_sequence_parts(&key);
                        has_sequences |= parts.len() > 1;
                        for part in parts {
                            keys.insert(part.to_string());
                        }
                    }
                }
            }
        }
    }

    unsafe { libmpv2_sys::mpv_free_node_contents(&mut node) };

    set_bound_mpv_keys(keys, has_sequences);
}

fn is_active_user_binding(is_weak: bool, priority: i64) -> bool {
    !is_weak && priority >= 0
}

fn binding_sequence_parts(key: &str) -> Vec<&str> {
    let bytes = key.as_bytes();
    let mut parts = Vec::new();
    let mut start = 0;

    while let Some(relative_end) = key[start..].find('-') {
        let mut end = start + relative_end;
        if end + 1 >= bytes.len() {
            break;
        }

        // mpv treats the second dash as the sequence separator in `Ctrl+--a`, leaving
        // `Ctrl+-` as the first key. Mirror input/keycodes.c here.
        if bytes[end + 1] == b'-' {
            end += 1;
        }
        parts.push(&key[start..end]);
        start = end + 1;
    }

    parts.push(&key[start..]);
    parts
}

fn set_property_async(name: &str, value: &PropVal, mpv: &Mpv) -> bool {
    // Synchronous client calls can wait for mpv's core during demuxer teardown,
    // starving this same thread of the events and commands needed to recover.
    let request_id = NEXT_MPV_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    let command_handle = MpvClientHandle(mpv.ctx.as_ptr());
    match run_mpv_set_property_async(command_handle, request_id, name, value) {
        Ok(()) => true,
        Err(error) => {
            eprintln!(
                "[MPV PROP ERROR] cannot queue {name}={value:?} request_id={request_id}: {error}"
            );
            false
        }
    }
}

fn request_property_async(mpv: &Mpv, name: &str, format: Format) -> bool {
    let request_id = NEXT_MPV_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    let command_handle = MpvClientHandle(mpv.ctx.as_ptr());
    match run_mpv_get_property_async(command_handle, request_id, name, format) {
        Ok(()) => true,
        Err(error) => {
            eprintln!(
                "[MPV PROP ERROR] cannot queue read for {name} request_id={request_id}: {error}"
            );
            false
        }
    }
}

fn mpv_error_string(error: i32) -> String {
    let ptr = unsafe { libmpv2_sys::mpv_error_string(error) };
    if ptr.is_null() {
        return format!("mpv error {error}");
    }
    unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

impl PlayerController {
    fn handle_in_msg(&mut self, mpv: &Mpv, msg: String, rpc_response_sender: &Sender<String>) {
        println!("[PLAYER] incoming raw message: {msg}");
        if let Ok(Value::Array(parts)) = serde_json::from_str::<Value>(&msg) {
            if parts.first().and_then(Value::as_str) == Some("shell-watch-party-sync") {
                if parts.len() != 2 {
                    eprintln!("[PLAYER] malformed local watch-party synchronization message");
                    return;
                }
                match serde_json::from_value::<LocalSyncCommand>(parts[1].clone()) {
                    Ok(command) => self.apply_local_sync(mpv, command),
                    Err(error) => {
                        eprintln!("[PLAYER] invalid local watch-party synchronization: {error}");
                    }
                }
                return;
            }
        }
        let in_msg: InMsg = match serde_json::from_str(&msg) {
            Ok(in_msg) => in_msg,
            Err(error) => {
                eprintln!("cannot parse InMsg:{:?} {error:#}", &msg);
                return;
            }
        };

        match in_msg {
            InMsg(InMsgFn::MpvObserveProp, InMsgArgs::ObProp(PropKey::Bool(prop))) => {
                observe_and_emit_current_property(
                    mpv,
                    rpc_response_sender,
                    &mut self.observed_properties,
                    &prop.to_string(),
                    Format::Flag,
                );
            }
            InMsg(InMsgFn::MpvObserveProp, InMsgArgs::ObProp(PropKey::Int(prop))) => {
                observe_and_emit_current_property(
                    mpv,
                    rpc_response_sender,
                    &mut self.observed_properties,
                    &prop.to_string(),
                    Format::Int64,
                );
            }
            InMsg(InMsgFn::MpvObserveProp, InMsgArgs::ObProp(PropKey::Fp(prop))) => {
                observe_and_emit_current_property(
                    mpv,
                    rpc_response_sender,
                    &mut self.observed_properties,
                    &prop.to_string(),
                    Format::Double,
                );
            }
            InMsg(InMsgFn::MpvObserveProp, InMsgArgs::ObProp(PropKey::Str(prop))) => {
                observe_and_emit_current_property(
                    mpv,
                    rpc_response_sender,
                    &mut self.observed_properties,
                    &prop.to_string(),
                    Format::String,
                );
            }
            InMsg(InMsgFn::MpvSetProp, InMsgArgs::StProp(name, PropVal::Bool(value))) => {
                let _ =
                    self.set_or_queue_prop(mpv, rpc_response_sender, name, PropVal::Bool(value));
            }
            InMsg(InMsgFn::MpvSetProp, InMsgArgs::StProp(name, PropVal::Num(value))) => {
                let _ = self.set_or_queue_prop(mpv, rpc_response_sender, name, PropVal::Num(value));
            }
            InMsg(InMsgFn::MpvSetProp, InMsgArgs::StProp(name, PropVal::Str(value))) => {
                let name_string = name.to_string();
                let is_vo = name_string == "vo";
                let value = if name_string == "sub-ass-override" && value == "strip" {
                    // Map "strip" to "scale". This preserves ASS styles and positioning while
                    // still allowing subtitle scaling.
                    "scale".to_string()
                } else if is_vo {
                    with_gpu_next_fallback(value)
                } else {
                    value
                };
                let changed =
                    self.set_or_queue_prop(mpv, rpc_response_sender, name, PropVal::Str(value));
                if is_vo && changed {
                    self.invalidate_display_output();
                }
            }
            InMsg(InMsgFn::MpvSetGpuVideoProcessing, InMsgArgs::Flag(enabled)) => {
                self.gpu_video_processing = enabled;
                self.invalidate_display_output();
                println!(
                    "[PLAYER] gpu video processing set to {enabled}; display refresh deferred"
                );
            }
            InMsg(InMsgFn::MpvCommand, InMsgArgs::Cmd(cmd)) => {
                self.handle_command(mpv, cmd);
            }
            msg => {
                eprintln!("MPV unsupported message: '{msg:?}'");
            }
        }
    }

    fn can_apply_local_sync(
        &self,
        command: &LocalSyncCommand,
        current_revision: u64,
        current_file_epoch: u64,
    ) -> bool {
        self.state == LoadState::Loaded
            && self.pending_media_command.is_none()
            && self.pending_loadfile.is_none()
            && self.loaded_file_epoch == Some(self.file_epoch)
            && self.file_epoch != 0
            && command.file_epoch == self.file_epoch
            && command.file_epoch == current_file_epoch
            && command.revision == current_revision
            && command
                .seek_to
                .is_none_or(|position| position.is_finite() && position >= 0.0)
    }

    fn apply_local_sync(&mut self, mpv: &Mpv, command: LocalSyncCommand) {
        if !self.can_apply_local_sync(
            &command,
            current_local_sync_revision(),
            FILE_LOAD_EPOCH.load(Ordering::Acquire),
        ) {
            return;
        }
        let _ = set_property_async("pause", &PropVal::Bool(command.paused), mpv);
        if let Some(position) = command.seek_to {
            // Check again after submitting pause: a new host selection can invalidate
            // this command while the player thread is handling it.
            if self.can_apply_local_sync(
                &command,
                current_local_sync_revision(),
                FILE_LOAD_EPOCH.load(Ordering::Acquire),
            ) {
                let _ = send_command(
                    mpv,
                    CmdVal::Tripple(MpvCmd::Seek, position.to_string(), "absolute".to_string()),
                );
            }
        }
    }

    fn reserve_file_epoch(&mut self) {
        self.file_epoch = FILE_LOAD_EPOCH
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        self.loaded_file_epoch = None;
        self.load_epoch_reserved = true;
        *IS_FILE_LOADED.lock().unwrap() = false;
        crate::stremio_app::steam_sync::notify_player_load_started(self.file_epoch);
    }

    fn handle_command(&mut self, mpv: &Mpv, cmd: CmdVal) {
        let cmd = match SubtitleCommand::from_command(cmd) {
            Ok(command) => {
                if self.state == LoadState::Loaded && self.pending_media_command.is_none() {
                    if let Some(sender) = &self.subtitle_request_sender {
                        let _ = sender.send(SubtitleWorkerRequest::Command(command));
                    }
                }
                return;
            }
            Err(cmd) => cmd,
        };
        if is_loadfile_command(&cmd) || is_stop_command(&cmd) {
            let load_id = self.video_ready.begin_transition();
            if is_loadfile_command(&cmd) {
                self.reserve_file_epoch();
            } else {
                self.loaded_file_epoch = None;
                self.load_epoch_reserved = false;
                *IS_FILE_LOADED.lock().unwrap() = false;
                crate::stremio_app::steam_sync::notify_player_unloaded(self.file_epoch);
            }
            if let Some(sender) = &self.rpc_response_sender {
                send_video_ready(sender, load_id, false);
            }
            if is_stop_command(&cmd) {
                self.pending_loadfile = None;
                self.pending_startup_retry = None;
                self.pending_reload_url = None;
                self.pending_reload_seek = None;
            }
            if let Some(sender) = &self.subtitle_request_sender {
                self.pending_media_command = Some(cmd);
                if self.pending_subtitle_ack.is_none() {
                    let (acknowledge, acknowledged) = flume::bounded(1);
                    if sender
                        .send(SubtitleWorkerRequest::BeginMediaChange(acknowledge))
                        .is_ok()
                    {
                        self.pending_subtitle_ack = Some(acknowledged);
                    } else {
                        eprintln!("mpv: subtitle worker stopped before media change");
                        self.pending_media_command = None;
                        self.load_epoch_reserved = false;
                        crate::stremio_app::steam_sync::notify_player_unloaded(self.file_epoch);
                    }
                }
                return;
            }
        }
        self.execute_command(mpv, cmd);
    }

    fn flush_pending_media_command(&mut self, mpv: &Mpv) {
        let Some(acknowledged) = &self.pending_subtitle_ack else {
            return;
        };
        match acknowledged.try_recv() {
            Err(flume::TryRecvError::Empty) => return,
            Err(flume::TryRecvError::Disconnected) => {
                eprintln!("mpv: subtitle worker stopped before media change");
                self.pending_subtitle_ack = None;
                self.pending_media_command = None;
                self.load_epoch_reserved = false;
                crate::stremio_app::steam_sync::notify_player_unloaded(self.file_epoch);
                return;
            }
            Ok(()) => {}
        }
        self.pending_subtitle_ack = None;
        if let Some(command) = self.pending_media_command.take() {
            self.execute_command(mpv, command);
        }
        if let Some(sender) = &self.subtitle_request_sender {
            let _ = sender.send(SubtitleWorkerRequest::EndMediaChange);
        }
    }

    fn execute_command(&mut self, mpv: &Mpv, cmd: CmdVal) {
        println!("[PLAYER] parsed mpv command: {cmd:?}");
        let cmd = sanitize_loadfile_start(cmd);
        println!("[PLAYER] sanitized mpv command: {cmd:?}");

        if is_stop_command(&cmd) {
            self.handle_stop(mpv, cmd);
            return;
        }

        if is_loadfile_command(&cmd) {
            self.capture_loadfile_url(&cmd);
            if self.state == LoadState::Stopping {
                println!("[PLAYER] deferring loadfile until stop completes: {cmd:?}");
                self.pending_loadfile = Some(cmd);
                return;
            }
            self.send_loadfile_now(mpv, cmd);
            return;
        }

        let _ = send_command(mpv, cmd);
    }

    fn set_or_queue_prop(
        &mut self,
        mpv: &Mpv,
        rpc_response_sender: &Sender<String>,
        name: PropKey,
        value: PropVal,
    ) -> bool {
        let name_string = name.to_string();
        apply_property_set_side_effect(&name_string, &value);
        if is_unchanged_persistent_render_property(
            &self.persistent_render_properties,
            &name_string,
            &value,
        ) {
            let value_json =
                normalize_property_value_for_web(&name_string, &prop_val_to_json(&value));
            cache_property_value(&name_string, &value_json);
            emit_property_value(rpc_response_sender, &name_string, value_json);
            println!("[PLAYER] skipping unchanged render property: {name_string}={value:?}");
            return false;
        }

        if self.should_defer_startup_property(&name) {
            let value_json =
                normalize_property_value_for_web(&name_string, &prop_val_to_json(&value));
            cache_property_value(&name_string, &value_json);
            emit_property_value(rpc_response_sender, &name_string, value_json);
            self.queue_startup_property(name, value);
            return true;
        }

        let applied = set_prop_val(name, value.clone(), mpv, Some(rpc_response_sender));
        if applied && is_persistent_render_property(&name_string) {
            self.persistent_render_properties.insert(name_string, value);
        }
        applied
    }

    fn handle_stop(&mut self, mpv: &Mpv, cmd: CmdVal) {
        if self.state == LoadState::Stopping {
            println!("[PLAYER] skipping duplicate stop while already stopping");
            return;
        }

        clear_file_scoped_cached_props();
        if self.state == LoadState::Idle {
            if self.abort_active_loadfile(mpv, "stop received while controller was idle") {
                println!("[PLAYER] cancelled unresolved loadfile from idle state");
            } else {
                println!("[PLAYER] skipping idle stop");
            }
            return;
        }

        let startup_load_in_flight = self
            .active_loadfile
            .as_ref()
            .map(|active| !active.start_file_seen)
            .unwrap_or(false);
        if self.state == LoadState::Loading
            && startup_load_in_flight
            && self.abort_active_loadfile(mpv, "stop received before StartFile")
        {
            println!("[PLAYER] cancelled startup loadfile without queuing stop");
            self.state = LoadState::Idle;
            self.stop_started_at = None;
            self.stop_command_request_id = None;
            *IS_FILE_LOADED.lock().unwrap() = false;
            *STOP_COMMAND_IN_FLIGHT.lock().unwrap() = false;
            return;
        }

        if self.state == LoadState::Loading
            && self.abort_active_loadfile(mpv, "stop received while loading")
        {
            println!("[PLAYER] cancelling in-flight loadfile with async stop");
            self.state = LoadState::Stopping;
            self.stop_started_at = Some(Instant::now());
            *IS_FILE_LOADED.lock().unwrap() = false;
            *STOP_COMMAND_IN_FLIGHT.lock().unwrap() = true;
            self.stop_command_request_id = send_command(mpv, cmd)
                .and_then(|submission| submission.abortable.then_some(submission.request_id));
            return;
        }

        println!("[PLAYER] entering Stopping state from {:?}", self.state);
        self.state = LoadState::Stopping;
        self.stop_started_at = Some(Instant::now());
        *IS_FILE_LOADED.lock().unwrap() = false;
        *STOP_COMMAND_IN_FLIGHT.lock().unwrap() = true;
        self.stop_command_request_id = send_command(mpv, cmd)
            .and_then(|submission| submission.abortable.then_some(submission.request_id));
    }

    fn capture_loadfile_url(&mut self, cmd: &CmdVal) {
        let Some(url) = loadfile_url(cmd) else {
            return;
        };

        let is_reload = self.pending_reload_url.as_deref() == Some(url)
            || (is_local_stream_url(url) && url.contains("_reload="));
        if !is_reload {
            if self.current_url == url {
                println!("[PLAYER] restarting current non-reload stream: {url}");
            } else {
                println!(
                    "[PLAYER] new non-reload stream. previous={}, next={}",
                    self.current_url, url
                );
            }
            clear_file_scoped_cached_props();
            *CURRENT_TIME.lock().unwrap() = 0.0;
            *TOTAL_DURATION.lock().unwrap() = 0.0;
            self.startup_retry_attempts = 0;
            self.command_queue_recoveries = 0;
            self.premature_eof_reloads = 0;
            self.pending_startup_retry = None;
            self.pending_reload_url = None;
            self.pending_reload_seek = None;
        }

        self.set_current_url(url.to_string());
        println!("Stream URL captured: {}", url);
    }

    fn on_start_file(&mut self, playlist_entry_id: i64) {
        self.video_ready.start_file(playlist_entry_id);
        if self.video_ready.active_load_id.is_some()
            && self.video_ready.active_load_id != self.video_ready.current_load_id
        {
            return;
        }
        if !self.load_epoch_reserved {
            // MPV can start another playlist item without a new shell load command.
            self.reserve_file_epoch();
        }
        self.load_epoch_reserved = false;
        self.state = LoadState::Loading;
        self.stop_started_at = None;
        if let Some(active) = self.active_loadfile.as_mut() {
            active.start_file_seen = true;
        }
        *CURRENT_TIME.lock().unwrap() = 0.0;
        *TOTAL_DURATION.lock().unwrap() = 0.0;
        *IS_FILE_LOADED.lock().unwrap() = false;
        *STOP_COMMAND_IN_FLIGHT.lock().unwrap() = false;
        clear_file_scoped_cached_props();
        *CURRENT_CHAPTER.lock().unwrap() = -1;
        *CURRENT_CHAPTER_COUNT.lock().unwrap() = 0;
        *CURRENT_CHAPTER_TITLE.lock().unwrap() = String::new();
    }

    fn on_file_loaded(&mut self, mpv: &Mpv, rpc_response_sender: &Sender<String>) -> bool {
        self.video_ready.file_loaded();
        if self.video_ready.active_load_id.is_some()
            && self.video_ready.active_load_id != self.video_ready.current_load_id
        {
            return false;
        }
        if self.file_epoch == 0 {
            self.reserve_file_epoch();
        }
        self.load_epoch_reserved = false;
        self.state = LoadState::Loaded;
        self.stop_started_at = None;
        self.stop_command_request_id = None;
        if let Some(active) = self.active_loadfile.as_mut() {
            active.file_loaded = true;
            self.active_loadfile = None;
        }
        self.pending_startup_retry = None;
        self.startup_retry_attempts = 0;
        self.loaded_file_epoch = Some(self.file_epoch);
        *IS_FILE_LOADED.lock().unwrap() = true;
        *STOP_COMMAND_IN_FLIGHT.lock().unwrap() = false;
        self.apply_pending_file_properties(mpv, rpc_response_sender);
        self.invalidate_display_output();
        crate::stremio_app::steam_sync::notify_player_loaded(self.file_epoch);
        true
    }

    fn on_end_file(&mut self) {
        let ended_current_file = self.file_epoch != 0
            && !self.load_epoch_reserved
            && self.video_ready.active_load_id == self.video_ready.current_load_id;
        self.video_ready.end_file();
        let replacement_is_loading = self
            .active_loadfile
            .as_ref()
            .map(|active| !active.file_loaded)
            .unwrap_or(false);
        if replacement_is_loading {
            println!(
                "[PLAYER] EndFile arrived while replacement loadfile is active; staying Loading"
            );
            self.state = LoadState::Loading;
        } else {
            self.state = LoadState::Idle;
        }
        self.stop_started_at = None;
        self.stop_command_request_id = None;
        self.loaded_file_epoch = None;
        *IS_FILE_LOADED.lock().unwrap() = false;
        *STOP_COMMAND_IN_FLIGHT.lock().unwrap() = false;
        if ended_current_file {
            crate::stremio_app::steam_sync::notify_player_unloaded(self.file_epoch);
        }
    }

    fn release_queued_transition_after_stop(&mut self, mpv: &Mpv) {
        self.state = LoadState::Idle;
        self.loaded_file_epoch = None;
        self.stop_started_at = None;
        self.stop_command_request_id = None;
        *IS_FILE_LOADED.lock().unwrap() = false;
        *STOP_COMMAND_IN_FLIGHT.lock().unwrap() = false;
        self.apply_pending_preload_properties(mpv);
        self.send_pending_loadfile(mpv);
    }

    fn should_defer_startup_property(&self, name: &PropKey) -> bool {
        (self.pending_media_command.is_some() && is_file_scoped_property(&name.to_string()))
            || should_defer_property(self.state, &name.to_string())
    }

    fn queue_startup_property(&mut self, name: PropKey, value: PropVal) {
        println!("[PLAYER] deferring property until a safe playback phase: {name:?}={value:?}");
        if let Some((_, existing_value)) = self
            .pending_startup_props
            .iter_mut()
            .find(|(existing_name, _)| *existing_name == name)
        {
            *existing_value = value;
            return;
        }
        self.pending_startup_props.push((name, value));
    }

    fn apply_pending_preload_properties(&mut self, mpv: &Mpv) {
        let pending = std::mem::take(&mut self.pending_startup_props);
        for (name, value) in pending {
            if is_file_scoped_property(&name.to_string()) {
                self.pending_startup_props.push((name, value));
                continue;
            }

            println!("[PLAYER] applying deferred pre-load property: {name:?}={value:?}");
            let name_string = name.to_string();
            if set_prop_val(name, value.clone(), mpv, None)
                && is_persistent_render_property(&name_string)
            {
                self.persistent_render_properties.insert(name_string, value);
            }
        }
    }

    fn apply_pending_file_properties(&mut self, mpv: &Mpv, rpc_response_sender: &Sender<String>) {
        let pending = std::mem::take(&mut self.pending_startup_props);
        for (name, value) in pending {
            println!("[PLAYER] applying deferred file property: {name:?}={value:?}");
            set_prop_val(name, value, mpv, Some(rpc_response_sender));
        }
    }

    fn take_pending_reload_seek_for(&mut self, url: &str) -> Option<f64> {
        match self.pending_reload_url.as_deref() {
            Some(expected_url) if expected_url == url => {
                self.pending_reload_url = None;
                self.pending_reload_seek.take()
            }
            // A stale FileLoaded event must not consume another file's resume position.
            Some(_) | None => None,
        }
    }

    fn recover_premature_end(
        &mut self,
        mpv: &Mpv,
        recovery: PrematureEndRecovery,
        url: &str,
        position: f64,
    ) -> bool {
        if self.premature_eof_reloads >= recovery.retry_limit() {
            return false;
        }

        let reload_url = recovery.reload_url(url);
        self.pending_reload_url = Some(reload_url.clone());
        self.pending_reload_seek = Some(position);
        self.handle_command(
            mpv,
            CmdVal::Tripple(MpvCmd::Loadfile, reload_url.clone(), "replace".to_string()),
        );

        if self.has_queued_loadfile_for(&reload_url) {
            self.premature_eof_reloads += 1;
            let origin = url::Url::parse(url)
                .map(|url| url.origin().ascii_serialization())
                .unwrap_or_else(|_| "unknown".to_string());
            println!(
                "[MPV] Recovering premature end ({recovery:?}), origin={origin}, attempt={}, position={position}",
                self.premature_eof_reloads
            );
            true
        } else {
            self.pending_reload_url = None;
            self.pending_reload_seek = None;
            false
        }
    }

    fn has_queued_loadfile_for(&self, url: &str) -> bool {
        self.active_loadfile
            .as_ref()
            .is_some_and(|active| active.url == url)
            || self.pending_loadfile.as_ref().and_then(loadfile_url) == Some(url)
            || self.pending_media_command.as_ref().and_then(loadfile_url) == Some(url)
    }

    fn set_current_url(&mut self, url: String) {
        self.current_url = url.clone();
        *CURRENT_STREAM_URL.lock().unwrap() = url;
    }

    fn send_loadfile_now(&mut self, mpv: &Mpv, cmd: CmdVal) {
        let url = loadfile_url(&cmd).unwrap_or_default().to_string();
        let requested_start = loadfile_requested_start(&cmd);
        let active_cmd = cmd.clone();
        self.abort_active_loadfile(mpv, "superseded by a newer loadfile");
        self.apply_pending_preload_properties(mpv);
        self.state = LoadState::Loading;
        self.stop_started_at = None;
        *IS_FILE_LOADED.lock().unwrap() = false;
        *STOP_COMMAND_IN_FLIGHT.lock().unwrap() = false;
        if let Some(submission) = send_command(mpv, cmd) {
            self.video_ready.queue_current_load(submission.request_id);
            self.active_loadfile = Some(ActiveLoadfile {
                cmd: active_cmd,
                url,
                command_request_id: submission.request_id,
                abortable: submission.abortable,
                queued_at: Instant::now(),
                last_wait_log_at: Instant::now(),
                command_replied: false,
                start_file_seen: false,
                file_loaded: false,
                requested_start,
            });
        } else {
            self.state = LoadState::Idle;
            self.load_epoch_reserved = false;
            crate::stremio_app::steam_sync::notify_player_unloaded(self.file_epoch);
        }
    }

    fn abort_active_loadfile(&mut self, mpv: &Mpv, reason: &str) -> bool {
        let Some(active) = self.active_loadfile.take() else {
            return false;
        };
        // Keep its readiness association until MPV replies. The load command may
        // already have executed even when its StartFile event is still queued.

        if active.abortable {
            println!(
                "[PLAYER] aborting active loadfile request_id={} url={} reason={}",
                active.command_request_id, active.url, reason
            );
            unsafe {
                libmpv2_sys::mpv_abort_async_command(mpv.ctx.as_ptr(), active.command_request_id);
            }
        } else {
            println!(
                "[PLAYER] dropping active sync loadfile request_id={} url={} reason={}",
                active.command_request_id, active.url, reason
            );
        }
        true
    }

    fn on_command_reply(&mut self, request_id: u64, mpv: &Mpv) {
        if self.stop_command_request_id == Some(request_id) {
            println!("[PLAYER] stop command reply request_id={request_id}");
            self.stop_command_request_id = None;
            if self.state == LoadState::Stopping {
                println!("[PLAYER] stop completed; releasing queued transition");
                self.release_queued_transition_after_stop(mpv);
            }
            return;
        }

        if let Some(active) = self.active_loadfile.as_mut() {
            if active.command_request_id == request_id {
                active.command_replied = true;
                println!(
                    "[PLAYER] loadfile command reply request_id={request_id} url={}",
                    active.url
                );
                return;
            }
        }

        println!("[PLAYER] async command reply request_id={request_id}");
    }

    fn send_pending_loadfile(&mut self, mpv: &Mpv) -> bool {
        let Some(cmd) = self.pending_loadfile.take() else {
            return false;
        };

        println!("[PLAYER] sending deferred loadfile after stop completed: {cmd:?}");
        self.send_loadfile_now(mpv, cmd);
        true
    }

    fn flush_stale_stop(&mut self, mpv: &Mpv) {
        if self.state != LoadState::Stopping {
            return;
        }

        let Some(started_at) = self.stop_started_at else {
            return;
        };

        if started_at.elapsed() < Duration::from_secs(2) {
            return;
        }

        println!("[PLAYER] stop did not produce EndFile quickly; releasing queued transition");
        if let Some(request_id) = self.stop_command_request_id.take() {
            println!("[PLAYER] aborting stale stop request_id={request_id}");
            unsafe {
                libmpv2_sys::mpv_abort_async_command(mpv.ctx.as_ptr(), request_id);
            }
        }

        self.release_queued_transition_after_stop(mpv);
    }

    fn flush_stale_loadfile(&mut self, mpv: &Mpv) {
        let Some(active) = self.active_loadfile.as_ref() else {
            return;
        };
        if active.start_file_seen {
            return;
        }

        let elapsed = active.queued_at.elapsed();
        if !active.command_replied && elapsed >= MPV_COMMAND_REPLY_TIMEOUT {
            if self.command_queue_recoveries >= MPV_COMMAND_QUEUE_RECOVERY_LIMIT {
                if let Some(active) = self.active_loadfile.as_mut() {
                    if active.last_wait_log_at.elapsed() >= LOCAL_LOADFILE_WAIT_LOG_INTERVAL {
                        active.last_wait_log_at = Instant::now();
                        println!(
                            "[MPV RECOVERY] loadfile command still has no reply after {}s; recovery limit exhausted for {}",
                            active.queued_at.elapsed().as_secs(),
                            active.url
                        );
                    }
                }
                return;
            }

            let url = active.url.clone();
            let retry_cmd = cache_busted_loadfile_cmd(active.cmd.clone());
            let retry_url = loadfile_url(&retry_cmd)
                .map(str::to_string)
                .unwrap_or_else(|| url.clone());
            self.command_queue_recoveries += 1;
            println!(
                "[MPV RECOVERY] loadfile command request_id={} produced no reply within {}s; recovery attempt {}/{} url={}",
                active.command_request_id,
                MPV_COMMAND_REPLY_TIMEOUT.as_secs(),
                self.command_queue_recoveries,
                MPV_COMMAND_QUEUE_RECOVERY_LIMIT,
                url
            );

            self.abort_active_loadfile(mpv, "loadfile command did not reply");
            self.pending_loadfile = Some(retry_cmd);
            self.set_current_url(retry_url);
            self.state = LoadState::Stopping;
            self.stop_started_at = Some(Instant::now());
            self.stop_command_request_id = None;
            *IS_FILE_LOADED.lock().unwrap() = false;
            *STOP_COMMAND_IN_FLIGHT.lock().unwrap() = true;
            self.release_queued_transition_after_stop(mpv);
            return;
        }

        if elapsed < LOADFILE_START_TIMEOUT {
            return;
        }

        let Some(active) = self.active_loadfile.as_mut() else {
            return;
        };
        if is_local_stream_url(&active.url) {
            if active.last_wait_log_at.elapsed() >= LOCAL_LOADFILE_WAIT_LOG_INTERVAL {
                active.last_wait_log_at = Instant::now();
                println!(
                    "[PLAYER] local stream still waiting for first bytes after {}s; keeping loadfile active: {}",
                    active.queued_at.elapsed().as_secs(),
                    active.url
                );
            }
            return;
        }

        let url = active.url.clone();
        let resume_position = active.requested_start;
        println!(
            "[PLAYER] loadfile produced no StartFile within {}s; cancelling url={}",
            LOADFILE_START_TIMEOUT.as_secs(),
            url
        );
        self.abort_active_loadfile(mpv, "StartFile watchdog expired");
        let _ = send_command(mpv, CmdVal::Single((MpvCmd::Stop,)));
        self.state = LoadState::Idle;

        if self.current_url != url {
            println!(
                "[PLAYER] not retrying stale watchdog URL. expired={}, current={}",
                url, self.current_url
            );
            return;
        }
        self.schedule_local_stream_startup_retry("StartFile watchdog", url, resume_position);
    }

    fn flush_pending_startup_retry(&mut self, mpv: &Mpv) {
        let Some(retry) = self.pending_startup_retry.as_ref() else {
            return;
        };

        if retry.due_at > Instant::now() {
            return;
        }

        let retry = self.pending_startup_retry.take().unwrap();
        if self.state == LoadState::Stopping || self.current_url != retry.expected_url {
            println!(
                "[MPV] Skipping stale startup retry attempt {}/{}. state={:?}, expected={}, current={}",
                retry.attempt,
                LOCAL_STREAM_STARTUP_RETRY_LIMIT,
                self.state,
                retry.expected_url,
                self.current_url
            );
            return;
        }

        println!(
            "[MPV] Running scheduled local stream retry attempt {}/{}: {}",
            retry.attempt, LOCAL_STREAM_STARTUP_RETRY_LIMIT, retry.retry_url
        );
        self.set_current_url(retry.retry_url.clone());
        if let Some(position) = retry.resume_position {
            self.pending_reload_url = Some(retry.retry_url.clone());
            self.pending_reload_seek = Some(position);
        }
        self.handle_command(
            mpv,
            CmdVal::Tripple(MpvCmd::Loadfile, retry.retry_url, "replace".to_string()),
        );
    }

    fn retry_local_stream_startup_failure(
        &mut self,
        source: &str,
        resume_position: Option<f64>,
    ) -> bool {
        let url = self.current_url.clone();
        let time = *CURRENT_TIME.lock().unwrap();
        let duration = *TOTAL_DURATION.lock().unwrap();
        let active_load_state = self
            .active_loadfile
            .as_ref()
            .map(|active| (active.start_file_seen, active.file_loaded));
        let startup_load_in_flight = matches!(active_load_state, Some((true, false)));

        if !should_retry_startup_failure_event(
            self.state,
            is_local_stream_url(&url),
            active_load_state,
        ) {
            println!(
                "[MPV] Not retrying startup failure ({source}): state={:?}, url={url}, time={time}, duration={duration}, startup_load_in_flight={startup_load_in_flight}",
                self.state
            );
            return false;
        }

        self.schedule_local_stream_startup_retry(source, url, resume_position)
    }

    fn schedule_local_stream_startup_retry(
        &mut self,
        source: &str,
        url: String,
        resume_position: Option<f64>,
    ) -> bool {
        let time = *CURRENT_TIME.lock().unwrap();
        let duration = *TOTAL_DURATION.lock().unwrap();

        if self.state == LoadState::Stopping || !is_local_stream_url(&url) {
            println!(
                "[MPV] Not scheduling startup retry ({source}): state={:?}, url={url}, time={time}, duration={duration}, resume_position={resume_position:?}",
                self.state
            );
            return false;
        }

        if self.startup_retry_attempts >= LOCAL_STREAM_STARTUP_RETRY_LIMIT {
            println!("[MPV] Not scheduling startup retry ({source}): attempts exhausted for {url}");
            return false;
        }

        self.startup_retry_attempts += 1;
        let attempt = self.startup_retry_attempts;

        println!(
            "[MPV] Local stream failed before playback ({source}). Scheduling retry in {}s (attempt {attempt}/{}, time={time}, duration={duration}, resume_position={resume_position:?})...",
            LOCAL_STREAM_STARTUP_RETRY_DELAY.as_secs(),
            LOCAL_STREAM_STARTUP_RETRY_LIMIT
        );

        let url2 = cache_busted_url(&url);
        self.pending_startup_retry = Some(ScheduledStartupRetry {
            due_at: Instant::now() + LOCAL_STREAM_STARTUP_RETRY_DELAY,
            expected_url: url,
            retry_url: url2,
            attempt,
            resume_position,
        });
        true
    }
}

fn is_stop_command(cmd: &CmdVal) -> bool {
    matches!(cmd, CmdVal::Single((MpvCmd::Stop,)))
}

fn is_loadfile_command(cmd: &CmdVal) -> bool {
    loadfile_url(cmd).is_some()
}

fn loadfile_url(cmd: &CmdVal) -> Option<&str> {
    match cmd {
        CmdVal::Double(MpvCmd::Loadfile, url)
        | CmdVal::Tripple(MpvCmd::Loadfile, url, ..)
        | CmdVal::Quadruple(MpvCmd::Loadfile, url, ..)
        | CmdVal::Quintuple(MpvCmd::Loadfile, url, ..) => Some(url),
        _ => None,
    }
}

fn loadfile_requested_start(cmd: &CmdVal) -> Option<f64> {
    match cmd {
        CmdVal::Quadruple(MpvCmd::Loadfile, _, _, options)
        | CmdVal::Quintuple(MpvCmd::Loadfile, _, _, _, options) => loadfile_start_seconds(options),
        _ => None,
    }
}

fn is_startup_property(name: &str) -> bool {
    matches!(
        name,
        "sub-scale"
            | "sub-pos"
            | "sub-color"
            | "sub-back-color"
            | "sub-border-color"
            | "sub-outline-color"
            | "sub-ass-override"
            | "hwdec"
            | "vo"
            | "osc"
            | "input-default-bindings"
            | "input-vo-keyboard"
            | "sid"
            | "aid"
            | "vid"
            | "pause"
            | "volume"
            | "mute"
            | "speed"
    )
}

fn is_file_scoped_property(name: &str) -> bool {
    matches!(name, "sid" | "aid" | "vid")
}

fn is_persistent_render_property(name: &str) -> bool {
    matches!(name, "vo" | "hwdec")
}

fn is_unchanged_persistent_render_property(
    properties: &HashMap<String, PropVal>,
    name: &str,
    value: &PropVal,
) -> bool {
    is_persistent_render_property(name) && properties.get(name) == Some(value)
}

fn should_defer_property(state: LoadState, name: &str) -> bool {
    if is_file_scoped_property(name) {
        return state != LoadState::Loaded;
    }

    state == LoadState::Stopping && is_startup_property(name)
}

fn should_retry_startup_failure_event(
    state: LoadState,
    is_local_stream: bool,
    active_load_state: Option<(bool, bool)>,
) -> bool {
    state == LoadState::Loading
        && is_local_stream
        && matches!(active_load_state, Some((true, false)))
}

fn is_local_stream_url(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && url.host().is_some_and(|host| match host {
                url::Host::Ipv4(address) => address.is_loopback(),
                url::Host::Ipv6(address) => address.is_loopback(),
                url::Host::Domain(name) => name == "localhost",
            })
    })
}

fn premature_end_recovery(
    state: LoadState,
    reason: libmpv2::EndFileReason,
    url: &str,
    time: f64,
    duration: f64,
    seekable: bool,
) -> Option<PrematureEndRecovery> {
    if !time.is_finite()
        || !duration.is_finite()
        || duration <= 30.0
        || time <= 0.0
        || time >= duration - 10.0
    {
        return None;
    }

    if is_local_stream_url(url) {
        return (reason == libmpv2::mpv_end_file_reason::Eof)
            .then_some(PrematureEndRecovery::Local);
    }

    (state == LoadState::Loaded
        && seekable
        && matches!(
            reason,
            libmpv2::mpv_end_file_reason::Eof | libmpv2::mpv_end_file_reason::Error
        )
        && url::Url::parse(url)
            .is_ok_and(|url| matches!(url.scheme(), "http" | "https") && url.has_host()))
    .then_some(PrematureEndRecovery::DirectHttp)
}

fn unrecovered_end_reason(
    reason: libmpv2::EndFileReason,
    recovery: Option<PrematureEndRecovery>,
) -> libmpv2::EndFileReason {
    if reason == libmpv2::mpv_end_file_reason::Eof
        && recovery == Some(PrematureEndRecovery::DirectHttp)
    {
        // Exhausted or failed recovery must not report a truncated VOD as completed.
        libmpv2::mpv_end_file_reason::Error
    } else {
        reason
    }
}

fn cache_busted_url(url: &str) -> String {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    cache_busted_url_at(url, timestamp)
}

fn cache_busted_loadfile_cmd(cmd: CmdVal) -> CmdVal {
    match cmd {
        CmdVal::Double(MpvCmd::Loadfile, url) => {
            CmdVal::Double(MpvCmd::Loadfile, cache_busted_url(&url))
        }
        CmdVal::Tripple(MpvCmd::Loadfile, url, flags) => {
            CmdVal::Tripple(MpvCmd::Loadfile, cache_busted_url(&url), flags)
        }
        CmdVal::Quadruple(MpvCmd::Loadfile, url, flags, index) => {
            CmdVal::Quadruple(MpvCmd::Loadfile, cache_busted_url(&url), flags, index)
        }
        CmdVal::Quintuple(MpvCmd::Loadfile, url, flags, index, options) => CmdVal::Quintuple(
            MpvCmd::Loadfile,
            cache_busted_url(&url),
            flags,
            index,
            options,
        ),
        cmd => cmd,
    }
}

fn cache_busted_url_at(url: &str, timestamp: u128) -> String {
    let Ok(mut parsed) = url::Url::parse(url) else {
        let separator = if url.contains('?') { "&" } else { "?" };
        return format!("{url}{separator}_reload={timestamp}");
    };

    let existing = parsed
        .query_pairs()
        .filter(|(name, _)| name != "_reload")
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    parsed.set_query(None);
    {
        let mut query = parsed.query_pairs_mut();
        for (name, value) in existing {
            query.append_pair(&name, &value);
        }
        query.append_pair("_reload", &timestamp.to_string());
    }
    parsed.into()
}

fn sanitize_loadfile_start(cmd: CmdVal) -> CmdVal {
    match cmd {
        CmdVal::Quintuple(MpvCmd::Loadfile, url, flags, index, options) => {
            let options = strip_stale_start_option(&url, options);
            if options.is_empty() {
                CmdVal::Tripple(MpvCmd::Loadfile, url, flags)
            } else {
                CmdVal::Quintuple(MpvCmd::Loadfile, url, flags, index, options)
            }
        }
        CmdVal::Quadruple(MpvCmd::Loadfile, url, flags, options) => {
            let options = strip_stale_start_option(&url, options);
            if options.is_empty() {
                CmdVal::Tripple(MpvCmd::Loadfile, url, flags)
            } else {
                CmdVal::Quadruple(MpvCmd::Loadfile, url, flags, options)
            }
        }
        cmd => cmd,
    }
}

fn strip_stale_start_option(url: &str, options: String) -> String {
    let Some(start) = loadfile_start_seconds(&options) else {
        return options;
    };

    let previous_url = CURRENT_STREAM_URL.lock().unwrap().clone();
    let previous_time = *CURRENT_TIME.lock().unwrap();
    let is_new_local_stream = !previous_url.is_empty()
        && previous_url != url
        && !url.contains("_reload=")
        && url.contains(":11470/");
    let mirrors_previous_position = previous_time > 30.0 && (start - previous_time).abs() <= 2.0;

    if !is_new_local_stream || !mirrors_previous_position {
        return options;
    }

    println!(
        "[MPV] Dropping stale loadfile start={} for new stream {}; previous stream was {} at {:.3}s",
        start, url, previous_url, previous_time
    );

    options
        .split(',')
        .filter(|option| !option.trim_start().starts_with("start="))
        .collect::<Vec<_>>()
        .join(",")
}

fn loadfile_start_seconds(options: &str) -> Option<f64> {
    options.split(',').find_map(|option| {
        let value = option.trim().strip_prefix("start=")?;
        value.trim_start_matches('+').parse::<f64>().ok()
    })
}

fn observe_property(mpv: &Mpv, name: &str, format: Format) {
    if let Err(error) = mpv.observe_property(name, format, 0) {
        eprintln!("failed to observe MPV property '{name}': {error:#}");
    }
}

fn observe_and_emit_current_property(
    mpv: &Mpv,
    rpc_response_sender: &Sender<String>,
    observed_properties: &mut HashSet<String>,
    name: &str,
    format: Format,
) {
    if observed_properties.insert(name.to_string()) {
        observe_property(mpv, name, format);
    } else {
        println!("[PLAYER] observer already registered: {name}");
    }
    if !should_emit_current_property(name) {
        println!("[PLAYER] immediate prop skipped: {name}");
        return;
    }
    emit_current_property(mpv, rpc_response_sender, name, format);
}

fn should_emit_current_property(name: &str) -> bool {
    // `@stremio/stremio-video` waits for mpv-version before it sends loadfile.
    // Emit cached stable values immediately and request missing values without
    // blocking the player event loop.
    is_cached_immediate_property(name)
}

fn is_cached_immediate_property(name: &str) -> bool {
    matches!(
        name,
        "mpv-version"
            | "ffmpeg-version"
            | "pause"
            | "volume"
            | "mute"
            | "sid"
            | "aid"
            | "vid"
            | "seekable"
            | "sub-scale"
            | "sub-pos"
            | "sub-delay"
            | "speed"
    )
}

fn emit_current_property(
    mpv: &Mpv,
    rpc_response_sender: &Sender<String>,
    name: &str,
    format: Format,
) {
    if let Some(value) = cached_property_value(name) {
        emit_property_value(rpc_response_sender, name, value);
        return;
    }

    let _ = request_property_async(mpv, name, format);
}

fn emit_property_value(rpc_response_sender: &Sender<String>, name: &str, value: Value) {
    println!("[PLAYER] immediate prop value: {name}={value}");
    let response = json!(["mpv-prop-change", { "name": name, "data": value }]);
    if let Err(error) = rpc_response_sender.send(RPCResponse::response_message(Some(response))) {
        eprintln!("[PLAYER] failed to emit immediate prop '{name}': {error:#}");
    }
}

fn cached_property_value(name: &str) -> Option<Value> {
    if let Some(value) = CACHED_PLAYER_PROPS.lock().unwrap().get(name).cloned() {
        return Some(value);
    }

    match name {
        "mpv-version" => MPV_VERSION.lock().unwrap().clone().map(Value::String),
        "ffmpeg-version" => FFMPEG_VERSION.lock().unwrap().clone().map(Value::String),
        "pause" => Some(json!(*IS_PAUSED.lock().unwrap())),
        "volume" => Some(json!(*CURRENT_VOLUME.lock().unwrap())),
        "mute" => Some(Value::Bool(false)),
        "sid" | "aid" | "vid" => Some(Value::String("no".to_string())),
        "sub-scale" => Some(json!(1.0)),
        "sub-pos" => Some(json!(100.0)),
        "sub-delay" => Some(json!(0.0)),
        "speed" => Some(json!(1.0)),
        _ => None,
    }
}

fn clear_file_scoped_cached_props() {
    let mut props = CACHED_PLAYER_PROPS.lock().unwrap();
    for name in [
        "time-pos",
        "duration",
        "seekable",
        "paused-for-cache",
        "cache-buffering-state",
        "demuxer-cache-time",
        "seeking",
        "eof-reached",
        "metadata",
        "video-params",
        "track-list",
        "aid",
        "vid",
        "sid",
    ] {
        props.remove(name);
    }
}

fn request_loaded_property_snapshot(mpv: &Mpv) {
    for (name, format) in [
        ("duration", Format::Double),
        ("seekable", Format::Flag),
        ("time-pos", Format::Double),
        ("pause", Format::Flag),
        ("volume", Format::Double),
        ("mute", Format::Flag),
        ("aid", Format::String),
        ("vid", Format::String),
        ("sid", Format::String),
    ] {
        let _ = request_property_async(mpv, name, format);
    }
}

fn emit_async_property_reply(
    rpc_response_sender: &Sender<String>,
    name: &str,
    result: &PropertyData,
) {
    let Some(value) = property_data_to_json(name, result) else {
        return;
    };
    if name == "duration" && value.as_f64().map(|value| value <= 0.0).unwrap_or(true) {
        return;
    }
    emit_loaded_snapshot_value(rpc_response_sender, name, value);
}

fn property_data_to_json(name: &str, value: &PropertyData) -> Option<Value> {
    match value {
        PropertyData::None => None,
        PropertyData::Flag(value) => Some(Value::Bool(*value)),
        PropertyData::Int64(value) => Some(json!(*value)),
        PropertyData::Double(value) if value.is_finite() => Some(json!(*value)),
        PropertyData::Double(_) => None,
        PropertyData::Str(value) | PropertyData::OsdStr(value) => {
            Some(mpv_string_property_to_json(name, value.to_string()))
        }
    }
}

fn emit_loaded_snapshot_value(rpc_response_sender: &Sender<String>, name: &str, value: Value) {
    let value = normalize_property_value_for_web(name, &value);
    cache_property_value(name, &value);
    emit_property_value(rpc_response_sender, name, value);
}

fn cache_property_value(name: &str, value: &Value) {
    let value = normalize_property_value_for_web(name, value);
    if is_cached_immediate_property(name) || name == "video-params" {
        CACHED_PLAYER_PROPS
            .lock()
            .unwrap()
            .insert(name.to_string(), value.clone());
    }

    match name {
        "mpv-version" => {
            if let Some(value) = value.as_str() {
                *MPV_VERSION.lock().unwrap() = Some(value.to_string());
            }
        }
        "ffmpeg-version" => {
            if let Some(value) = value.as_str() {
                *FFMPEG_VERSION.lock().unwrap() = Some(value.to_string());
            }
        }
        "volume" => {
            if let Some(volume) = value.as_f64() {
                *CURRENT_VOLUME.lock().unwrap() = volume;
            }
        }
        _ => {}
    }
}

fn normalize_property_value_for_web(name: &str, value: &Value) -> Value {
    if name == "mute" {
        if let Some(value) = value.as_str() {
            return Value::Bool(matches!(value, "yes" | "true" | "1"));
        }
    }
    value.clone()
}

fn mpv_string_property_to_json(name: &str, value: String) -> Value {
    if matches!(name, "metadata" | "track-list" | "video-params") {
        match serde_json::from_str::<Value>(&value) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("[PLAYER] failed to parse JSON mpv property '{name}': {error:#}");
                Value::String(value)
            }
        }
    } else {
        Value::String(value)
    }
}

fn update_cached_property(name: &str, change: &PropertyData) {
    let cached_value = match change {
        PropertyData::None => None,
        PropertyData::Flag(value) => Some(Value::Bool(*value)),
        PropertyData::Int64(value) => Some(json!(*value)),
        PropertyData::Double(value) => Some(json!(*value)),
        PropertyData::Str(value) | PropertyData::OsdStr(value) => {
            Some(mpv_string_property_to_json(name, value.to_string()))
        }
    };

    if let Some(value) = cached_value {
        cache_property_value(name, &value);
    }

    if name == "volume" {
        if let PropertyData::Double(volume) = change {
            *CURRENT_VOLUME.lock().unwrap() = *volume;
            shell_state::save_volume_debounced(*volume);
        }
    }
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
    if name == "chapters" {
        if let PropertyData::Int64(chapter_count) = change {
            *CURRENT_CHAPTER_COUNT.lock().unwrap() = *chapter_count;
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

fn send_command(mpv: &Mpv, cmd: CmdVal) -> Option<CommandSubmission> {
    let (name, args) = command_parts(cmd);
    send_command_parts(mpv, name, args)
}

fn send_command_parts(mpv: &Mpv, name: String, args: Vec<String>) -> Option<CommandSubmission> {
    let request_id = NEXT_MPV_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    println!(
        "[PLAYER] dispatching mpv command: {} {:?} request_id={} mode=async",
        name, args, request_id
    );
    let command_handle = MpvClientHandle(mpv.ctx.as_ptr());
    let started_at = Instant::now();

    println!("[MPV CMD START] {name} {args:?} request_id={request_id} mode=async");
    match run_mpv_command_async(command_handle, request_id, &name, &args) {
        Ok(()) => {
            println!(
                "[MPV CMD QUEUED] {} {:?} request_id={} elapsed_ms={} mode=async",
                name,
                args,
                request_id,
                started_at.elapsed().as_millis()
            );
            Some(CommandSubmission {
                request_id,
                abortable: true,
            })
        }
        Err(error) => {
            eprintln!(
                "[MPV CMD ERROR] {} {:?} request_id={} elapsed_ms={} mode=async: '{}'",
                name,
                args,
                request_id,
                started_at.elapsed().as_millis(),
                error
            );
            None
        }
    }
}

fn command_parts(cmd: CmdVal) -> (String, Vec<String>) {
    match cmd {
        CmdVal::Quintuple(name, arg1, arg2, arg3, arg4) => {
            (name.to_string(), vec![arg1, arg2, arg3, arg4])
        }
        CmdVal::Quadruple(name, arg1, arg2, arg3) => (name.to_string(), vec![arg1, arg2, arg3]),
        CmdVal::Tripple(name, arg1, arg2) => (name.to_string(), vec![arg1, arg2]),
        CmdVal::Double(name, arg1) => (name.to_string(), vec![arg1]),
        CmdVal::Single((name,)) => (name.to_string(), Vec::new()),
    }
}

fn set_prop_val(
    name: PropKey,
    value: PropVal,
    mpv: &Mpv,
    rpc_response_sender: Option<&Sender<String>>,
) -> bool {
    let name = name.to_string();
    let value_json = normalize_property_value_for_web(&name, &prop_val_to_json(&value));
    let ok = set_property_async(&name, &value, mpv);

    if !ok {
        return false;
    }

    apply_property_set_side_effect(&name, &value);
    cache_property_value(&name, &value_json);
    if let Some(rpc_response_sender) = rpc_response_sender {
        emit_property_value(rpc_response_sender, &name, value_json);
    }
    true
}

fn apply_property_set_side_effect(name: &str, value: &PropVal) {
    if name != "pause" {
        return;
    }

    let pause = match value {
        PropVal::Bool(value) => Some(*value),
        PropVal::Num(value) => Some(*value != 0.0),
        PropVal::Str(value) => match value.as_str() {
            "yes" | "true" | "1" => Some(true),
            "no" | "false" | "0" => Some(false),
            _ => None,
        },
    };

    if let Some(pause) = pause {
        *IS_PAUSED.lock().unwrap() = pause;
        cache_property_value(name, &Value::Bool(pause));
    }
}

fn prop_val_to_json(value: &PropVal) -> Value {
    match value {
        PropVal::Bool(value) => Value::Bool(*value),
        PropVal::Num(value) => json!(*value),
        PropVal::Str(value) => Value::String(value.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        binding_sequence_parts, cache_busted_url_at, is_active_user_binding,
        is_unchanged_persistent_render_property, property_data_to_json, should_defer_property,
        should_retry_startup_failure_event, LoadState, PropVal, VideoReadyState,
    };
    use libmpv2::events::PropertyData;
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn local_watch_sync_requires_the_current_loaded_file() {
        use super::{CmdVal, LocalSyncCommand, MpvCmd, PlayerController};

        let mut controller = PlayerController::new(0);
        controller.file_epoch = 41;
        let command = LocalSyncCommand {
            revision: 17,
            file_epoch: 41,
            paused: true,
            seek_to: Some(120.0),
        };
        assert!(!controller.can_apply_local_sync(&command, 17, 41));
        controller.state = LoadState::Loading;
        assert!(!controller.can_apply_local_sync(&command, 17, 41));
        controller.state = LoadState::Loaded;
        assert!(!controller.can_apply_local_sync(&command, 17, 41));
        controller.loaded_file_epoch = Some(41);
        assert!(controller.can_apply_local_sync(&command, 17, 41));

        controller.pending_media_command = Some(CmdVal::Double(
            MpvCmd::Loadfile,
            "https://guest.example/new-source".to_string(),
        ));
        assert!(!controller.can_apply_local_sync(&command, 17, 41));
        controller.pending_media_command = None;
        controller.pending_loadfile = Some(CmdVal::Double(
            MpvCmd::Loadfile,
            "https://guest.example/new-source".to_string(),
        ));
        assert!(!controller.can_apply_local_sync(&command, 17, 41));
        controller.pending_loadfile = None;
        controller.state = LoadState::Stopping;
        assert!(!controller.can_apply_local_sync(&command, 17, 41));
    }

    #[test]
    fn local_watch_sync_rejects_obsolete_revisions_epochs_and_invalid_seeks() {
        use super::{LocalSyncCommand, PlayerController};

        let mut controller = PlayerController::new(0);
        controller.state = LoadState::Loaded;
        controller.file_epoch = 52;
        controller.loaded_file_epoch = Some(52);
        let mut command = LocalSyncCommand {
            revision: 29,
            file_epoch: 52,
            paused: false,
            seek_to: None,
        };
        assert!(controller.can_apply_local_sync(&command, 29, 52));
        assert!(!controller.can_apply_local_sync(&command, 30, 52));
        assert!(!controller.can_apply_local_sync(&command, 29, 53));
        command.file_epoch = 51;
        assert!(!controller.can_apply_local_sync(&command, 29, 52));
        command.file_epoch = 52;
        for position in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0] {
            command.seek_to = Some(position);
            assert!(!controller.can_apply_local_sync(&command, 29, 52));
        }
        command.seek_to = Some(0.0);
        assert!(controller.can_apply_local_sync(&command, 29, 52));

        // A same-URL reload belongs to a new file epoch even when the route and
        // URL stay unchanged. Commands waiting for the previous file must expire.
        controller.file_epoch = 53;
        controller.loaded_file_epoch = None;
        assert!(!controller.can_apply_local_sync(&command, 29, 53));
        controller.loaded_file_epoch = Some(53);
        assert!(!controller.can_apply_local_sync(&command, 29, 53));
    }

    #[test]
    fn local_stream_recovery_accepts_random_runtime_ports() {
        for url in [
            "http://127.0.0.1:45821/stream",
            "http://localhost:45822/stream",
            "http://[::1]:45823/stream",
        ] {
            assert!(super::is_local_stream_url(url));
        }
        for url in [
            "http://example.com:11470/stream",
            "http://127.0.0.1.evil.test:11470/stream",
            "file:///C:/video.mp4",
        ] {
            assert!(!super::is_local_stream_url(url));
        }
    }

    #[test]
    fn premature_http_end_requires_seekable_loaded_vod() {
        use libmpv2::mpv_end_file_reason::{Eof, Error, Quit, Redirect, Stop};

        for reason in [Eof, Error] {
            for url in [
                "http://cdn.example/video",
                "https://cdn.example/video?token=a%2Fb",
            ] {
                assert_eq!(
                    super::premature_end_recovery(
                        LoadState::Loaded,
                        reason,
                        url,
                        120.0,
                        7200.0,
                        true,
                    ),
                    Some(super::PrematureEndRecovery::DirectHttp),
                );
            }
        }
        for state in [LoadState::Idle, LoadState::Loading, LoadState::Stopping] {
            assert_eq!(
                super::premature_end_recovery(
                    state,
                    Eof,
                    "https://cdn.example/video",
                    120.0,
                    7200.0,
                    true,
                ),
                None,
            );
        }
        for reason in [Stop, Quit, Redirect] {
            assert_eq!(
                super::premature_end_recovery(
                    LoadState::Loaded,
                    reason,
                    "https://cdn.example/video",
                    120.0,
                    7200.0,
                    true,
                ),
                None,
            );
        }
        for url in ["file:///C:/video.mp4", "rtsp://cdn.example/live", "invalid"] {
            assert_eq!(
                super::premature_end_recovery(LoadState::Loaded, Eof, url, 120.0, 7200.0, true,),
                None,
            );
        }
        assert_eq!(
            super::premature_end_recovery(
                LoadState::Loaded,
                Eof,
                "https://cdn.example/live",
                120.0,
                7200.0,
                false,
            ),
            None,
        );
    }

    #[test]
    fn premature_http_end_excludes_unknown_duration_and_real_completion() {
        use libmpv2::mpv_end_file_reason::Eof;

        for (time, duration) in [
            (120.0, 0.0),
            (1.0, 30.0),
            (0.0, 7200.0),
            (-1.0, 7200.0),
            (7190.0, 7200.0),
            (7200.0, 7200.0),
            (7201.0, 7200.0),
            (f64::NAN, 7200.0),
            (f64::INFINITY, 7200.0),
            (120.0, f64::NAN),
            (120.0, f64::INFINITY),
        ] {
            assert_eq!(
                super::premature_end_recovery(
                    LoadState::Loaded,
                    Eof,
                    "https://cdn.example/video",
                    time,
                    duration,
                    true,
                ),
                None,
                "time={time}, duration={duration}",
            );
        }
    }

    #[test]
    fn premature_http_reload_preserves_signed_url_and_local_retry_policy() {
        use super::PrematureEndRecovery;
        use libmpv2::mpv_end_file_reason::{Eof, Error};

        let signed_url = "https://CDN.example:443/a%2fb?token=a%2Fb+z&x=1&x=2&_reload=signed#part";
        assert_eq!(
            PrematureEndRecovery::DirectHttp.reload_url(signed_url),
            signed_url
        );
        assert_eq!(PrematureEndRecovery::DirectHttp.retry_limit(), 3);
        assert_eq!(PrematureEndRecovery::Local.retry_limit(), 1);
        let local = "http://127.0.0.1:45821/hash/3";
        assert!(PrematureEndRecovery::Local
            .reload_url(local)
            .starts_with(local));
        assert!(PrematureEndRecovery::Local
            .reload_url(local)
            .contains("?_reload="));
        assert_eq!(
            super::premature_end_recovery(LoadState::Loaded, Eof, local, 120.0, 7200.0, false),
            Some(PrematureEndRecovery::Local),
        );
        assert_eq!(
            super::premature_end_recovery(LoadState::Loaded, Error, local, 120.0, 7200.0, true),
            None,
        );
    }

    #[test]
    fn premature_http_recovery_budget_and_resume_survive_same_url_reload() {
        use super::{CmdVal, PlayerController, PrematureEndRecovery};
        use libmpv2::Mpv;

        let mpv = Mpv::with_initializer(|init| {
            init.set_property("vo", "null")?;
            init.set_property("ao", "null")?;
            init.set_property("config", "no")?;
            Ok(())
        })
        .unwrap();
        let url = "https://cdn.example/video?token=a%2fb+z&x=1&x=2";
        let mut controller = PlayerController::new(0);
        let (subtitle_tx, _subtitle_rx) = flume::unbounded();
        let (rpc_tx, _rpc_rx) = flume::unbounded();
        controller.subtitle_request_sender = Some(subtitle_tx);
        controller.set_current_url(url.to_string());

        for attempt in 1..=PrematureEndRecovery::DirectHttp.retry_limit() {
            let position = 120.0 * f64::from(attempt);
            assert!(controller.recover_premature_end(
                &mpv,
                PrematureEndRecovery::DirectHttp,
                url,
                position,
            ));
            assert_eq!(controller.premature_eof_reloads, attempt);
            let command: CmdVal = controller.pending_media_command.take().unwrap();
            controller.pending_subtitle_ack = None;
            assert_eq!(super::loadfile_url(&command), Some(url));
            controller.capture_loadfile_url(&command);
            controller.on_end_file();
            controller.on_start_file(0);
            assert!(controller.on_file_loaded(&mpv, &rpc_tx));
            assert_eq!(controller.premature_eof_reloads, attempt);
            assert_eq!(controller.pending_reload_seek, Some(position));
            assert_eq!(
                controller.take_pending_reload_seek_for("https://old.example/video"),
                None
            );
            assert_eq!(controller.take_pending_reload_seek_for(url), Some(position));
            assert_eq!(controller.take_pending_reload_seek_for(url), None);
        }
        assert!(!controller.recover_premature_end(
            &mpv,
            PrematureEndRecovery::DirectHttp,
            url,
            600.0,
        ));
        assert!(controller.pending_media_command.is_none());
        assert!(controller.pending_reload_seek.is_none());

        // A provider-owned _reload parameter does not identify a shell recovery.
        controller.capture_loadfile_url(&CmdVal::Double(
            super::MpvCmd::Loadfile,
            "https://cdn.example/other?_reload=signed&token=a%2Fb".to_string(),
        ));
        assert_eq!(controller.premature_eof_reloads, 0);
    }

    #[test]
    fn failed_premature_http_recovery_keeps_error_visible() {
        use super::{PlayerController, PrematureEndRecovery};
        use libmpv2::{
            mpv_end_file_reason::{Eof, Error},
            Mpv,
        };

        let mpv = Mpv::with_initializer(|init| {
            init.set_property("vo", "null")?;
            init.set_property("ao", "null")?;
            init.set_property("config", "no")?;
            Ok(())
        })
        .unwrap();
        let mut controller = PlayerController::new(0);
        let (subtitle_tx, subtitle_rx) = flume::unbounded();
        drop(subtitle_rx);
        controller.subtitle_request_sender = Some(subtitle_tx);
        assert!(!controller.recover_premature_end(
            &mpv,
            PrematureEndRecovery::DirectHttp,
            "https://cdn.example/video",
            120.0,
        ));
        assert_eq!(controller.premature_eof_reloads, 0);
        assert!(controller.pending_reload_url.is_none());
        assert!(controller.pending_reload_seek.is_none());
        assert_eq!(
            super::unrecovered_end_reason(Eof, Some(PrematureEndRecovery::DirectHttp)),
            Error
        );
        assert_eq!(
            super::unrecovered_end_reason(Eof, Some(PrematureEndRecovery::Local)),
            Eof
        );
        assert_eq!(super::unrecovered_end_reason(Eof, None), Eof);
    }

    #[test]
    fn premature_http_recovery_cannot_reuse_previous_files_seekable_property() {
        super::cache_property_value("seekable", &json!(true));
        assert_eq!(super::cached_property_value("seekable"), Some(json!(true)));
        super::clear_file_scoped_cached_props();
        assert_eq!(super::cached_property_value("seekable"), None);
    }

    #[test]
    fn unavailable_mpv_properties_preserve_premature_end_recovery_state() {
        *super::CURRENT_TIME.lock().unwrap() = 4.9;
        *super::TOTAL_DURATION.lock().unwrap() = 40.0;
        super::cache_property_value("seekable", &json!(true));

        for name in ["time-pos", "duration", "seekable"] {
            super::update_cached_property(name, &PropertyData::None);
            assert_eq!(property_data_to_json(name, &PropertyData::None), None);
        }
        assert_eq!(*super::CURRENT_TIME.lock().unwrap(), 4.9);
        assert_eq!(*super::TOTAL_DURATION.lock().unwrap(), 40.0);
        assert_eq!(super::cached_property_value("seekable"), Some(json!(true)));
        assert_eq!(
            super::premature_end_recovery(
                LoadState::Loaded,
                libmpv2::mpv_end_file_reason::Eof,
                "https://cdn.example/video",
                *super::CURRENT_TIME.lock().unwrap(),
                *super::TOTAL_DURATION.lock().unwrap(),
                super::cached_property_value("seekable").and_then(|value| value.as_bool())
                    == Some(true),
            ),
            Some(super::PrematureEndRecovery::DirectHttp),
        );
        super::clear_file_scoped_cached_props();
    }

    #[test]
    fn raw_event_adapter_preserves_failed_http_end_reason() {
        use super::{wait_player_event, PlayerMpvEvent};
        use libmpv2::{mpv_end_file_reason::Error, Mpv};
        use std::net::TcpListener;
        use std::time::{Duration, Instant};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let mpv = Mpv::with_initializer(|init| {
            init.set_property("vo", "null")?;
            init.set_property("ao", "null")?;
            init.set_property("config", "no")?;
            Ok(())
        })
        .unwrap();
        mpv.command("loadfile", &[&format!("http://{address}/video.mp4")])
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            match wait_player_event(&mpv, 0.01) {
                Some(Ok(PlayerMpvEvent::EndFile(reason))) => {
                    assert_eq!(reason, Error);
                    return;
                }
                Some(Err(error)) => panic!("failed HTTP EndFile lost its reason: {}", error),
                _ => {}
            }
        }
        panic!("failed HTTP load did not emit EndFile");
    }

    #[test]
    fn readiness_waits_for_file_load_and_emits_once() {
        let mut ready = VideoReadyState::default();
        let id = ready.begin_transition();
        ready.queue_current_load(1);
        ready.command_replied(1, Some(101));
        assert_eq!(ready.playback_restarted(), None);
        ready.start_file(101);
        assert_eq!(ready.playback_restarted(), None);
        ready.file_loaded();
        assert_eq!(ready.playback_restarted(), Some(id));
        assert_eq!(ready.playback_restarted(), None);
        ready.end_file();
        assert_eq!(ready.playback_restarted(), None);
    }

    #[test]
    fn obsolete_playback_and_stop_cannot_report_ready_for_new_media() {
        let mut ready = VideoReadyState::default();
        ready.begin_transition();
        ready.queue_current_load(1);
        ready.command_replied(1, Some(101));
        ready.start_file(101);
        let replacement = ready.begin_transition();
        ready.file_loaded();
        assert_eq!(ready.playback_restarted(), None);
        ready.queue_current_load(2);
        ready.command_replied(2, Some(102));
        ready.start_file(102);
        ready.file_loaded();
        assert_eq!(ready.playback_restarted(), Some(replacement));
        ready.begin_transition();
        assert_eq!(ready.playback_restarted(), None);
    }

    #[test]
    fn aborted_unstarted_load_does_not_consume_the_next_files_readiness() {
        let mut ready = VideoReadyState::default();
        ready.begin_transition();
        ready.queue_current_load(1);
        ready.command_replied(1, None);
        let replacement = ready.begin_transition();
        ready.queue_current_load(2);
        ready.command_replied(2, Some(102));
        ready.start_file(102);
        ready.file_loaded();
        assert_eq!(ready.playback_restarted(), Some(replacement));
    }

    #[test]
    fn raw_event_adapter_preserves_owned_scalar_properties() {
        use super::{wait_player_event, OwnedPropertyData, PlayerMpvEvent};
        use libmpv2::{Format, Mpv};
        use std::time::{Duration, Instant};
        let mpv = Mpv::with_initializer(|init| {
            init.set_property("vo", "null")?;
            init.set_property("ao", "null")?;
            init.set_property("config", "no")?;
            init.set_property("pause", true)?;
            init.set_property("title", "adapter-owned-value")?;
            init.set_property("volume", 37.5)?;
            Ok(())
        })
        .unwrap();
        for (index, (name, format)) in [
            ("title", Format::String),
            ("pause", Format::Flag),
            ("volume", Format::Double),
            ("playlist-pos", Format::Int64),
        ]
        .iter()
        .enumerate()
        {
            mpv.observe_property(name, *format, index as u64 + 1)
                .unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut values = HashMap::new();
        while Instant::now() < deadline && values.len() < 4 {
            if let Some(Ok(PlayerMpvEvent::PropertyChange(name, value))) =
                wait_player_event(&mpv, 0.01)
            {
                values.insert(name, value);
            }
        }
        // Values remain valid after every later MPV wait has recycled its event memory.
        assert!(
            matches!(values.get("title"), Some(OwnedPropertyData::Str(value)) if value == "adapter-owned-value")
        );
        assert!(matches!(
            values.get("pause"),
            Some(OwnedPropertyData::Flag(true))
        ));
        assert!(
            matches!(values.get("volume"), Some(OwnedPropertyData::Double(value)) if *value == 37.5)
        );
        assert!(matches!(
            values.get("playlist-pos"),
            Some(OwnedPropertyData::Int64(-1))
        ));
    }

    #[test]
    fn queued_old_mpv_events_cannot_report_new_files_readiness() {
        use super::{
            run_mpv_command_async, wait_player_event, LocalSyncCommand, MpvClientHandle,
            PlayerController, PlayerMpvEvent,
        };
        use libmpv2::{events::Event, Mpv};
        use std::time::{Duration, Instant};
        let mpv = Mpv::with_initializer(|init| {
            init.set_property("vo", "null")?;
            init.set_property("ao", "null")?;
            init.set_property("config", "no")?;
            init.set_property("pause", true)?;
            Ok(())
        })
        .unwrap();
        // This observer waits for A while the controller's event queue stays unread.
        let observer = mpv.create_client(None).unwrap();
        let mut controller = PlayerController::new(0);
        let (rpc_sender, _) = flume::unbounded();
        let ready = &mut controller.video_ready;
        ready.begin_transition();
        let handle = MpvClientHandle(mpv.ctx.as_ptr());
        run_mpv_command_async(
            handle,
            11,
            "loadfile",
            &["av://lavfi:sine=frequency=440:duration=10".to_owned()],
        )
        .unwrap();
        ready.queue_current_load(11);
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut loaded = false;
        while Instant::now() < deadline {
            if matches!(observer.wait_event(0.01), Some(Ok(Event::FileLoaded))) {
                loaded = true;
                break;
            }
        }
        assert!(loaded, "old file did not load");
        let replacement = ready.begin_transition();
        unsafe {
            libmpv2_sys::mpv_abort_async_command(mpv.ctx.as_ptr(), 11);
        }
        run_mpv_command_async(
            handle,
            12,
            "loadfile",
            &["av://lavfi:sine=frequency=550:duration=10".to_owned()],
        )
        .unwrap();
        ready.queue_current_load(12);
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut reports = Vec::new();
        let mut starts = 0;
        while Instant::now() < deadline {
            match wait_player_event(&mpv, 0.01) {
                Some(Ok(PlayerMpvEvent::CommandReply {
                    request_id,
                    playlist_entry_id,
                    ..
                })) => controller
                    .video_ready
                    .command_replied(request_id, playlist_entry_id),
                Some(Ok(PlayerMpvEvent::StartFile(entry_id))) => {
                    starts += 1;
                    controller.on_start_file(entry_id);
                }
                Some(Ok(PlayerMpvEvent::FileLoaded)) => {
                    let current = controller.on_file_loaded(&mpv, &rpc_sender);
                    assert_eq!(
                        current,
                        starts == 2,
                        "old FileLoaded must not change the current controller state"
                    );
                    let current_command = LocalSyncCommand {
                        revision: 73,
                        file_epoch: controller.file_epoch,
                        paused: true,
                        seek_to: Some(2.0),
                    };
                    assert_eq!(
                        controller.can_apply_local_sync(
                            &current_command,
                            73,
                            controller.file_epoch,
                        ),
                        current,
                        "queued old FileLoaded cannot authorize guest synchronization",
                    );
                    if current {
                        let stale_command = LocalSyncCommand {
                            file_epoch: controller.file_epoch - 1,
                            ..current_command
                        };
                        assert!(!controller.can_apply_local_sync(
                            &stale_command,
                            73,
                            controller.file_epoch,
                        ));
                    }
                }
                Some(Ok(PlayerMpvEvent::EndFile(_))) => controller.on_end_file(),
                Some(Ok(PlayerMpvEvent::PlaybackRestart)) => {
                    if let Some(id) = controller.video_ready.playback_restarted() {
                        reports.push(id);
                    }
                    if starts == 2 {
                        break;
                    }
                }
                Some(Err(error)) => panic!("MPV fixture event failed: {}", error),
                _ => {}
            }
        }
        assert_eq!(starts, 2, "both queued files must have started");
        assert_eq!(reports, vec![replacement]);
    }

    #[test]
    fn subtitle_worker_cancels_stalled_download_before_media_change() {
        use super::{create_subtitle_thread, SubtitleCommand, SubtitleWorkerRequest};
        use libmpv2::{events::Event, Mpv};
        use std::io::Write;
        use std::net::TcpListener;
        use std::time::{Duration, Instant};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let (connected_tx, connected_rx) = flume::bounded(1);
        let (release_tx, release_rx) = flume::bounded(1);
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if let Ok((mut stream, _)) = listener.accept() {
                    connected_tx.send(()).unwrap();
                    let _ = release_rx.recv_timeout(Duration::from_secs(5));
                    let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        let mpv = Mpv::with_initializer(|init| {
            init.set_property("vo", "null")?;
            init.set_property("ao", "null")?;
            init.set_property("config", "no")?;
            init.set_property("pause", true)?;
            Ok(())
        })
        .unwrap();
        mpv.command("loadfile", &["av://lavfi:sine=frequency=440:duration=10"])
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut loaded = false;
        while Instant::now() < deadline {
            if matches!(mpv.wait_event(0.01), Some(Ok(Event::FileLoaded))) {
                loaded = true;
                break;
            }
        }
        assert!(loaded, "test audio fixture did not load");
        let (requests_tx, requests_rx) = flume::unbounded();
        let worker = create_subtitle_thread(mpv.create_client(None).unwrap(), requests_rx);
        requests_tx
            .send(SubtitleWorkerRequest::Command(SubtitleCommand {
                name: "sub-add",
                args: vec![
                    "sub-add".to_string(),
                    format!("http://{address}/stall.ass"),
                    "auto".to_string(),
                    "Test".to_string(),
                    "eng".to_string(),
                ],
            }))
            .unwrap();
        connected_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("subtitle download did not start");
        let (ack_tx, ack_rx) = flume::bounded(1);
        requests_tx
            .send(SubtitleWorkerRequest::BeginMediaChange(ack_tx))
            .unwrap();
        let acknowledgement = ack_rx.recv_timeout(Duration::from_secs(3));
        let _ = release_tx.send(());
        drop(requests_tx);
        server.join().unwrap();
        worker.join().unwrap();
        assert!(
            acknowledgement.is_ok(),
            "stalled subtitle prevented media change"
        );
    }

    #[test]
    fn cache_buster_adds_query_to_plain_stream_url() {
        assert_eq!(
            cache_busted_url_at("http://127.0.0.1:11470/hash/3", 42),
            "http://127.0.0.1:11470/hash/3?_reload=42"
        );
    }

    #[test]
    fn cache_buster_replaces_only_its_own_query_parameter() {
        assert_eq!(
            cache_busted_url_at(
                "http://127.0.0.1:11470/hash/3?tr=tracker%3Audp%3A%2F%2Fhost&_reload=1",
                42,
            ),
            "http://127.0.0.1:11470/hash/3?tr=tracker%3Audp%3A%2F%2Fhost&_reload=42"
        );
    }

    #[test]
    fn input_binding_sequences_are_split_like_mpv() {
        assert_eq!(binding_sequence_parts("a-b-c"), ["a", "b", "c"]);
        assert_eq!(binding_sequence_parts("-"), ["-"]);
        assert_eq!(binding_sequence_parts("Ctrl+-"), ["Ctrl+-"]);
        assert_eq!(binding_sequence_parts("Ctrl+--a"), ["Ctrl+-", "a"]);
    }

    #[test]
    fn only_active_user_input_bindings_are_forwarded() {
        assert!(is_active_user_binding(false, 0));
        assert!(is_active_user_binding(false, 10));
        assert!(!is_active_user_binding(true, 10));
        assert!(!is_active_user_binding(false, -1));
    }

    #[test]
    fn global_properties_apply_before_loading() {
        for property in ["vo", "hwdec", "pause", "volume", "mute", "speed"] {
            assert!(!should_defer_property(LoadState::Idle, property));
            assert!(!should_defer_property(LoadState::Loading, property));
        }
    }

    #[test]
    fn global_properties_wait_for_an_active_stop() {
        for property in ["vo", "hwdec", "pause", "volume", "mute", "speed"] {
            assert!(should_defer_property(LoadState::Stopping, property));
        }
    }

    #[test]
    fn repeated_render_setup_is_coalesced_without_coalescing_playback_controls() {
        let properties = HashMap::from([
            ("vo".to_string(), PropVal::Str("gpu-next,gpu,".to_string())),
            ("pause".to_string(), PropVal::Bool(false)),
        ]);

        assert!(is_unchanged_persistent_render_property(
            &properties,
            "vo",
            &PropVal::Str("gpu-next,gpu,".to_string()),
        ));
        assert!(!is_unchanged_persistent_render_property(
            &properties,
            "vo",
            &PropVal::Str("gpu,".to_string()),
        ));
        assert!(!is_unchanged_persistent_render_property(
            &properties,
            "pause",
            &PropVal::Bool(false),
        ));
    }

    #[test]
    fn track_selection_waits_for_file_loaded() {
        for property in ["sid", "aid", "vid"] {
            assert!(should_defer_property(LoadState::Idle, property));
            assert!(should_defer_property(LoadState::Loading, property));
            assert!(should_defer_property(LoadState::Stopping, property));
            assert!(!should_defer_property(LoadState::Loaded, property));
        }
    }

    #[test]
    fn startup_recovery_requires_a_started_local_load() {
        assert!(should_retry_startup_failure_event(
            LoadState::Loading,
            true,
            Some((true, false)),
        ));
        assert!(!should_retry_startup_failure_event(
            LoadState::Loading,
            true,
            Some((false, false)),
        ));
        assert!(!should_retry_startup_failure_event(
            LoadState::Loaded,
            true,
            Some((true, true)),
        ));
        assert!(!should_retry_startup_failure_event(
            LoadState::Loading,
            false,
            Some((true, false)),
        ));
    }

    #[test]
    fn async_property_replies_preserve_structured_video_params() {
        assert_eq!(
            property_data_to_json("video-params", &PropertyData::Str(r#"{"h":1080}"#)),
            Some(json!({ "h": 1080 }))
        );
        assert_eq!(
            property_data_to_json("duration", &PropertyData::Double(f64::NAN)),
            None
        );
    }
}
