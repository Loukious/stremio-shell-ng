use crate::stremio_app::constants::SERVER_IPC_KEY;
use crate::stremio_app::ipc;
use crate::stremio_app::stremio_player::player::PLAYER_CMD_TX;
use native_windows_gui::{self as nwg, PartialUi};
use once_cell::sync::Lazy;
use once_cell::unsync::OnceCell;
use serde_json::json;
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};
use std::mem;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::thread;
use url::Url;
use urlencoding::decode;
use webview2::Controller;
use webview2_sys::KeyEventKind;
use winapi::shared::windef::HWND;
use winapi::um::winuser::{GetClientRect, GetKeyState, VK_F7, WM_APPCOMMAND, WM_SETFOCUS};

const APPCOMMAND_MEDIA_NEXTTRACK: u32 = 11;
const APPCOMMAND_MEDIA_PREVIOUSTRACK: u32 = 12;
const APPCOMMAND_MEDIA_PLAY_PAUSE: u32 = 14;
const APPCOMMAND_MEDIA_PLAY: u32 = 46;
const APPCOMMAND_MEDIA_PAUSE: u32 = 47;
pub static WEB_CMD_TX: Lazy<Mutex<Option<flume::Sender<String>>>> = Lazy::new(|| Mutex::new(None));
pub const WEBVIEW_EXEC_SCRIPT_PREFIX: &str = "__stremio_shell_exec_script__:";

fn send_mpv_keypress(key: &str) {
    let message = json!(["mpv-command", ["keypress", key]]).to_string();

    if let Ok(guard) = PLAYER_CMD_TX.lock() {
        if let Some(player_tx) = guard.as_ref() {
            player_tx.send(message).ok();
        }
    }
}

#[derive(Default)]
struct MpvKeyBindings {
    keys: HashSet<String>,
    has_sequences: bool,
}

static BOUND_MPV_KEYS: Lazy<Mutex<MpvKeyBindings>> =
    Lazy::new(|| Mutex::new(MpvKeyBindings::default()));

/// Replace the active user binding keys discovered from mpv's normalized
/// `input-bindings` property.
pub fn set_bound_mpv_keys(keys: HashSet<String>, has_sequences: bool) {
    if keys.is_empty() {
        println!("[MPV KEYS] no active user bindings found");
    } else {
        println!("[MPV KEYS] bound keys: {keys:?}");
    }

    if let Ok(mut bindings) = BOUND_MPV_KEYS.lock() {
        bindings.keys = keys;
        bindings.has_sequences = has_sequences;
    }

    // This is normally populated before WebView construction. Also push an update when the
    // WebView already exists so future runtime refreshes cannot leave JavaScript with stale keys.
    let script = build_mpv_keydown_script();
    if let Ok(tx) = WEB_CMD_TX.lock() {
        if let Some(tx) = tx.as_ref() {
            tx.send(format!("{WEBVIEW_EXEC_SCRIPT_PREFIX}{script}"))
                .ok();
        }
    }
}

fn is_key_bound(mpv_key: &str) -> bool {
    BOUND_MPV_KEYS
        .lock()
        .map(|bindings| bindings.keys.contains(mpv_key))
        .unwrap_or(false)
}

fn is_key_down(virtual_key: i32) -> bool {
    unsafe { GetKeyState(virtual_key) as u16 & 0x8000 != 0 }
}

/// Build the normalized key names for browser-reserved shortcuts. Printable
/// keys encode Shift in the produced character, while symbolic keys retain it
/// as a modifier.
fn browser_reserved_mpv_key(virtual_key: u32) -> Option<String> {
    browser_reserved_mpv_key_with_modifiers(
        virtual_key,
        is_key_down(0x10),
        is_key_down(0x11),
        is_key_down(0x12),
        is_key_down(0x5B) || is_key_down(0x5C),
    )
}

fn browser_reserved_mpv_key_with_modifiers(
    virtual_key: u32,
    shift: bool,
    ctrl: bool,
    alt: bool,
    meta: bool,
) -> Option<String> {
    let is_symbolic = virtual_key == VK_F7 as u32;
    let mut base = match virtual_key {
        key if key == VK_F7 as u32 => "F7".to_string(),
        0x46 => "f".to_string(),
        0x47 => "g".to_string(),
        _ => return None,
    };

    if !is_symbolic && shift {
        base.make_ascii_uppercase();
    }

    let mut key = String::with_capacity(24);
    if is_symbolic && shift {
        key.push_str("Shift+");
    }
    if ctrl {
        key.push_str("Ctrl+");
    }
    if alt {
        key.push_str("Alt+");
    }
    if meta {
        key.push_str("Meta+");
    }
    key.push_str(&base);
    Some(key)
}

/// Returns `true` when the current webview URL is on the Stremio player page.
fn is_on_player_page() -> bool {
    CURRENT_URL
        .lock()
        .map(|url| url.contains("/player/"))
        .unwrap_or(false)
}

const PIP_BUTTON_SCRIPT: &str = r##"
;(function(){
    if (window.self !== window.top) return;
    if (window.__stremioShellPipInstalled) return;
    window.__stremioShellPipInstalled = true;

    var PIP_INNER = ''
        + '<rect x="100" y="126" width="312" height="260" rx="40" ry="40" '
        +   'style="stroke:currentcolor;stroke-width:34;fill:none"></rect>'
        + '<rect x="238" y="240" width="150" height="104" rx="16" ry="16" '
        +   'style="fill:currentcolor;stroke:none"></rect>';

    function isPlayerRoute(){
        return (location.hash || '').indexOf('/player') !== -1;
    }

    function findFullscreenBtn(){
        var els = document.querySelectorAll('[title]');
        for (var i = 0; i < els.length; i++){
            var title = (els[i].getAttribute('title') || '').toLowerCase();
            if (title.indexOf('fullscreen') !== -1){
                var rect = els[i].getBoundingClientRect();
                if (rect.width > 0 && rect.height > 0) return els[i];
            }
        }
        return null;
    }

    function buildPipButton(fsBtn){
        var pip = fsBtn.cloneNode(true);
        pip.id = 'stremio-shell-pip-btn';
        pip.setAttribute('title', 'Picture-in-Picture');
        pip.removeAttribute('aria-label');
        var svg = pip.querySelector('svg');
        if (svg) svg.innerHTML = PIP_INNER;
        pip.addEventListener('click', function(e){
            e.stopPropagation();
            e.preventDefault();
            try {
                window.chrome.webview.postMessage(JSON.stringify({
                    id: 1,
                    args: ['win-set-pip', {}]
                }));
            } catch (_) {}
        }, true);
        return pip;
    }

    function placeButton(){
        if (!isPlayerRoute()) return;
        var fsBtn = findFullscreenBtn();
        if (!fsBtn || !fsBtn.parentElement) return;
        var pip = document.getElementById('stremio-shell-pip-btn');
        if (pip && pip.parentElement === fsBtn.parentElement && pip.nextElementSibling === fsBtn) {
            return;
        }
        if (!pip || !pip.isConnected) pip = buildPipButton(fsBtn);
        fsBtn.parentElement.insertBefore(pip, fsBtn);
    }

    window.addEventListener('hashchange', placeButton);
    setInterval(placeButton, 500);
    placeButton();

    try {
        var observer = new MutationObserver(function(){ placeButton(); });
        observer.observe(document.documentElement, { childList: true, subtree: true });
    } catch (_) {}

    try {
        window.chrome.webview.addEventListener('message', function(ev){
            try {
                var data = typeof ev.data === 'string' ? JSON.parse(ev.data) : ev.data;
                var args = data && data.args;
                if (Array.isArray(args) && args[0] === 'win-pip-changed') {
                    var enabled = !!(args[1] && args[1].enabled);
                    var button = document.getElementById('stremio-shell-pip-btn');
                    if (button) {
                        button.setAttribute('title', enabled ? 'Exit Picture-in-Picture' : 'Picture-in-Picture');
                    }
                }
            } catch (_) {}
        });
    } catch (_) {}
})();
"##;

/// Build the JavaScript that translates layout-aware `keydown` events into
/// mpv key names. It forwards active bindings and, when sequences exist, the
/// intervening events mpv needs to maintain or clear its sequence history.
///
/// The bound-key set from `input.conf` is serialised into the script so
/// the JS side can check membership without a round-trip to Rust.
fn build_mpv_keydown_script() -> String {
    let (mut keys, has_sequences) = BOUND_MPV_KEYS
        .lock()
        .map(|bindings| {
            (
                bindings.keys.iter().cloned().collect::<Vec<_>>(),
                bindings.has_sequences,
            )
        })
        .unwrap_or_default();
    keys.sort();
    let keys_json = serde_json::to_string(&keys).unwrap_or_else(|_| "[]".to_string());

    format!(
        r##";(function(){{
    if (window.self !== window.top) return;
    window.__stremioShellMpvBoundKeys = new Set({keys});
    window.__stremioShellMpvHasSequences = {has_sequences};
    if (window.__stremioShellMpvKeysInstalled) return;
    window.__stremioShellMpvKeysInstalled = true;

    var KEY_MAP = {{
        ' ':         'SPACE',
        'Enter':     'ENTER',
        'Tab':       'TAB',
        'Escape':    'ESC',
        'Backspace': 'BS',
        'Delete':    'DEL',
        'Insert':    'INS',
        'Home':      'HOME',
        'End':       'END',
        'PageUp':    'PGUP',
        'PageDown':  'PGDWN',
        'ArrowLeft': 'LEFT',
        'ArrowRight':'RIGHT',
        'ArrowUp':   'UP',
        'ArrowDown': 'DOWN',
        '#':         'SHARP'
    }};

    var NUMPAD_MAP = {{
        'Numpad0': 'KP0', 'Numpad1': 'KP1', 'Numpad2': 'KP2',
        'Numpad3': 'KP3', 'Numpad4': 'KP4', 'Numpad5': 'KP5',
        'Numpad6': 'KP6', 'Numpad7': 'KP7', 'Numpad8': 'KP8',
        'Numpad9': 'KP9', 'NumpadDecimal': 'KP_DEC',
        'NumpadEnter': 'KP_ENTER', 'NumpadAdd': 'KP_ADD',
        'NumpadSubtract': 'KP_SUBTRACT', 'NumpadMultiply': 'KP_MULTIPLY',
        'NumpadDivide': 'KP_DIVIDE'
    }};

    function isPlayerRoute() {{
        return (location.hash || '').indexOf('/player') !== -1;
    }}

    function isTextInput(el) {{
        if (!el) return false;
        var tag = el.tagName;
        if (tag === 'TEXTAREA') return true;
        if (tag === 'INPUT') {{
            var t = (el.type || '').toLowerCase();
            return ['text','password','number','search','email','url','tel'].indexOf(t) !== -1;
        }}
        return el.isContentEditable === true;
    }}

    function eventToMpvKey(e) {{
        var base = null;
        var isTextKey = false;

        if (e.location === 3 && NUMPAD_MAP[e.code]) {{
            base = NUMPAD_MAP[e.code];
        }} else {{
            var characters = Array.from(e.key || '');
            isTextKey = characters.length === 1;
            if (isTextKey) {{
                base = KEY_MAP[e.key] || e.key;
            }} else if (KEY_MAP[e.key]) {{
                base = KEY_MAP[e.key];
            }} else if (/^F([1-9]|1[0-9]|2[0-4])$/.test(e.key)) {{
                base = e.key;
            }}
        }}

        if (!base) return null;

        var altGraph = false;
        try {{ altGraph = e.getModifierState('AltGraph'); }} catch (_) {{}}

        var result = '';
        // mpv removes Shift from produced text keys (A, @, Unicode characters), but retains it
        // for symbolic keys such as Shift+LEFT.
        if (!isTextKey && e.shiftKey) result += 'Shift+';
        if (e.ctrlKey && !altGraph) result += 'Ctrl+';
        if (e.altKey && !altGraph) result += 'Alt+';
        if (e.metaKey) result += 'Meta+';
        return result + base;
    }}

    document.addEventListener('keydown', function(e) {{
        if (!isPlayerRoute()) return;
        if (isTextInput(e.target)) return;

        var mpvKey = eventToMpvKey(e);
        if (!mpvKey) return;

        var boundKeys = window.__stremioShellMpvBoundKeys || new Set();
        var isBound = boundKeys.has(mpvKey);
        if (!isBound && !window.__stremioShellMpvHasSequences) return;

        try {{
            window.chrome.webview.postMessage(JSON.stringify({{
                id: 1,
                args: ['mpv-command', ['keypress', mpvKey]]
            }}));
        }} catch(_) {{}}

        if (isBound) {{
            e.preventDefault();
            e.stopPropagation();
        }}
    }}, true);
}})();"##,
        keys = keys_json,
        has_sequences = has_sequences
    )
}

use super::constants::{WARNING_URL, WHITELISTED_HOSTS};

pub static CURRENT_URL: Lazy<Mutex<String>> = Lazy::new(|| Mutex::new("".to_string()));

#[derive(Default)]
pub struct WebView {
    pub endpoint: Rc<OnceCell<String>>,
    pub dev_tools: Rc<OnceCell<bool>>,
    pub controller: Rc<OnceCell<Controller>>,
    pub channel: ipc::Channel,
    notice: nwg::Notice,
    compute: RefCell<Option<thread::JoinHandle<()>>>,
    message_queue: Arc<Mutex<VecDeque<String>>>,
}

impl WebView {
    pub fn fit_to_window(&self, hwnd: Option<HWND>) {
        if let Some(hwnd) = hwnd {
            unsafe {
                let mut rect = mem::zeroed();
                GetClientRect(hwnd, &mut rect);
                self.controller
                    .get()
                    .and_then(|controller| controller.put_bounds(rect).ok());
            }
        }
    }

    fn resize_to_window_bounds(controller: Option<&Controller>, hwnd: Option<HWND>) {
        if let (Some(controller), Some(hwnd)) = (controller, hwnd) {
            unsafe {
                let mut rect = mem::zeroed();
                GetClientRect(hwnd, &mut rect);
                controller.put_bounds(rect).ok();
            }
        }
    }
}

impl PartialUi for WebView {
    fn build_partial<W: Into<nwg::ControlHandle>>(
        data: &mut Self,
        parent: Option<W>,
    ) -> Result<(), nwg::NwgError> {
        println!("Building WebView");
        let (tx, rx) = flume::unbounded();
        let tx_drag_drop = tx.clone();
        let (tx_web, rx_web) = flume::unbounded();
        let tx_fs = tx_web.clone();
        data.channel = RefCell::new(Some((tx, rx_web)));

        let parent = parent.ok_or_else(|| nwg::NwgError::no_parent("WebView"))?;
        let parent = parent.into();

        let hwnd = parent
            .hwnd()
            .ok_or_else(|| nwg::NwgError::control_create("Cannot obtain WebView window handle"))?;
        nwg::Notice::builder()
            .parent(parent)
            .build(&mut data.notice)
            .ok();
        let controller_clone = data.controller.clone();
        let endpoint = data.endpoint.clone();
        let dev_tools = data.dev_tools.clone();
        let webview_flags = "--autoplay-policy=no-user-gesture-required \
        --disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection,CalculateNativeWinOcclusion \
        --disable-backgrounding-occluded-windows \
        --disable-renderer-backgrounding \
        --disable-background-timer-throttling";
        let result = webview2::EnvironmentBuilder::new()
            .with_additional_browser_arguments(webview_flags)
            .build(move |env| {
                env.expect("Cannot obtain webview environment")
                    .create_controller(hwnd, move |controller| {
                        let controller = controller.expect("Cannot obtain webview controller");
                        if let Ok(controller2) = controller.get_controller2() {
                            controller2
                                .put_default_background_color(webview2_sys::Color {
                                    r: 0,
                                    g: 0,
                                    b: 0,
                                    a: 0,
                                })
                                .ok();
                        } else {
                            eprintln!("failed to get interface to controller2");
                        }
                    let webview = controller
                            .get_webview()
                            .expect("Cannot obtain webview from controller");
                    let settings = webview.get_settings().unwrap();
                    settings.put_is_status_bar_enabled(false).ok();
                    settings.put_are_dev_tools_enabled(*dev_tools.get().unwrap()).ok();
                    settings.put_is_zoom_control_enabled(false).ok();
                    settings.put_is_built_in_error_page_enabled(false).ok();
                    settings.put_are_host_objects_allowed(false).ok();
                    settings.put_are_default_script_dialogs_enabled(false).ok();

                    // Handle window.open and href
                    webview.add_new_window_requested(move |_webview, event| {
                        if let Ok(uri) = event.get_uri() {
                            if let Ok(url) = Url::parse(&uri) {
                                let is_whitelisted = url.host().is_some_and(|host| {
                                    WHITELISTED_HOSTS.iter().any(|whitelisted_host| host.to_string().ends_with(whitelisted_host))
                                });

                                let final_url = if is_whitelisted {
                                    url.to_string()
                                } else {
                                    format!("{}{}", WARNING_URL, urlencoding::encode(url.as_ref()))
                                };

                                if let Err(e) = open::that(final_url) {
                                    eprintln!("Failed to open URL: {e}");
                                }
                            }
                        }

                        Ok(())
                    })?;

                    if let Some(endpoint) = endpoint.get() {
                        if webview
                            .navigate(endpoint.as_str()).is_err() {
                                tx_web.clone().send(ipc::RPCResponse::response_message(Some(json!(["app-error", format!("Cannot load WEB UI at '{}'", &endpoint)])))).ok();
                        };
                    }
                        webview.add_web_message_received(move |_w, msg| {
                            let msg = msg.try_get_web_message_as_string()?;
                            tx_web.send(msg).ok();
                            Ok(())
                        }).expect("Cannot add web message received");
                        webview.add_new_window_requested(move |_w, msg| {
                            if let Some(file) = msg.get_uri().ok().and_then(|str| {decode(str.as_str()).ok().map(Cow::into_owned)}) {
                                tx_drag_drop.send(ipc::RPCResponse::response_message(Some(json!(["dragdrop" ,[file]])))).ok();
                                msg.put_handled(true).ok();
                            }
                            Ok(())
                        }).expect("Cannot add D&D handler");
                        webview.add_contains_full_screen_element_changed(move |wv| {
                            if let Ok(visibility) = wv.get_contains_full_screen_element() {
                                tx_fs.send(ipc::RPCResponse::response_message(Some(json!(["win-set-visibility" , {"fullscreen": visibility}])))).ok();
                            }
                            Ok(())
                        }).expect("Cannot add full screen element changed");

                        webview.add_source_changed(move |webview, _args| {
                            if let Ok(new_src) = webview.get_source() {
                                *CURRENT_URL.lock().unwrap() = new_src;
                            }
                            Ok(())
                        }).expect("Cannot add source_changed event");

                        webview.add_content_loading(move |wv, _| {
                            wv.execute_script(format!(
                                    "window.stremio_server_ipc_key='{}'",
                                    std::env::var(SERVER_IPC_KEY).unwrap_or_default()
                            ).as_str(), |_| Ok(())
                            ).expect("Cannot add SERVER_IPC_KEY to webview");

                            wv.execute_script(r##"
                            try{
                                /* Disable context menus */
                                document.addEventListener('contextmenu', (e) => {
                                    if(!(e.target.tagName == "INPUT" &&
                                    ['text', 'password', 'number', 'week', 'month', 'email'].includes(e.target.type.toLowerCase()))) {
                                        e.stopPropagation();e.preventDefault()
                                    }
                                    })
                            }catch(e){}

                            try{console.log('Shell JS injected');if(window.self === window.top) {
                                window.qt={webChannelTransport:{send:window.chrome.webview.postMessage}};
                                window.chrome.webview.addEventListener('message',ev=>window.qt.webChannelTransport.onmessage(ev));
                                }}catch(e){}
                            window.addEventListener("load", function() {if(initShellComm) try { initShellComm() } catch(e) {}}, false)

                            try{
                                if(window.self === window.top && !window.__stremioShellRouteReporterInstalled) {
                                    window.__stremioShellRouteReporterInstalled = true;
                                    let backgroundPlayerFocusHeld = false;
                                    const isPlayerRoute = () =>
                                        (window.location.hash || '').indexOf('/player/') !== -1;
                                    const keepBackgroundPlayerActive = () => {
                                        if (!isPlayerRoute()) {
                                            backgroundPlayerFocusHeld = false;
                                            return;
                                        }
                                        if (document.hasFocus()) {
                                            backgroundPlayerFocusHeld = false;
                                            return;
                                        }
                                        if (backgroundPlayerFocusHeld) return;

                                        // The Web UI's useRouteFocused() unsubscribes every model when
                                        // document.hasFocus() is false. Keep the embedded player subscribed
                                        // in the background without activating the native window.
                                        backgroundPlayerFocusHeld = true;
                                        window.dispatchEvent(new Event('focus'));
                                    };
                                    const reportRoute = () => {
                                        setTimeout(keepBackgroundPlayerActive, 0);
                                        try {
                                            window.chrome.webview.postMessage(JSON.stringify({
                                                id: 1,
                                                args: ["shell-route-changed", window.location.href]
                                            }));
                                        } catch(e) {}
                                    };
                                    const wrapHistory = (name) => {
                                        const original = window.history[name];
                                        window.history[name] = function() {
                                            const result = original.apply(this, arguments);
                                            setTimeout(reportRoute, 0);
                                            return result;
                                        };
                                    };
                                    wrapHistory('pushState');
                                    wrapHistory('replaceState');
                                    window.addEventListener('popstate', reportRoute);
                                    window.addEventListener('hashchange', reportRoute);
                                    window.addEventListener('blur', () => {
                                        backgroundPlayerFocusHeld = false;
                                        setTimeout(keepBackgroundPlayerActive, 0);
                                    });
                                    window.addEventListener('focus', (event) => {
                                        if (event.isTrusted) backgroundPlayerFocusHeld = false;
                                    }, true);
                                    reportRoute();
                                }
                            }catch(e){}
                            "##, |_| Ok(())).expect("Cannot add script to webview");
                            wv.execute_script(PIP_BUTTON_SCRIPT, |_| Ok(()))
                                .expect("Cannot add PiP button script to webview");
                            let mpv_keydown_js = build_mpv_keydown_script();
                            wv.execute_script(&mpv_keydown_js, |_| Ok(()))
                                .expect("Cannot add mpv keydown script to webview");
                            Ok(())
                        }).expect("Cannot add content loading");

                        WebView::resize_to_window_bounds(Some(&controller), Some(hwnd));
                        controller.put_is_visible(true).ok();
                        controller
                            .move_focus(webview2::MoveFocusReason::Programmatic)
                            .ok();
                        controller.add_accelerator_key_pressed(move |_, e| {
                            let k = e.get_virtual_key()?;
                            let event_kind = e.get_key_event_kind()?;
                            let ctrl_down = is_key_down(0x11);
                            let is_browser_reserved =
                                k == VK_F7 as u32 || ctrl_down && matches!(k, 0x46 | 0x47);

                            if !is_browser_reserved {
                                // Unhandled accelerator keys continue to the document keydown
                                // listener, which has layout-aware KeyboardEvent.key data.
                                return Ok(());
                            }

                            let is_keydown = event_kind == KeyEventKind::KeyDown
                                || event_kind == KeyEventKind::SystemKeyDown;
                            if is_keydown && is_on_player_page() {
                                if let Some(mpv_key) = browser_reserved_mpv_key(k) {
                                    if is_key_bound(&mpv_key) {
                                        send_mpv_keypress(&mpv_key);
                                    }
                                }
                            }

                            // Keep WebView find/caret-browsing disabled. If the shortcut is bound,
                            // the keydown above routes it exclusively to mpv first.
                            e.put_handled(true)
                        })
                        .unwrap();

                        controller_clone
                            .set(controller)
                            .expect("Cannot update the controller");
                        Ok(())
                    })
            });
        if let Err(e) = result {
            nwg::modal_fatal_message(
                parent,
                "Failed to Create WebView2 Environment",
                &format!("{e}"),
            );
        }

        let sender = data.notice.sender();
        let message = data.message_queue.clone();
        *data.compute.borrow_mut() = Some(thread::spawn(move || loop {
            if let Ok(msg) = rx.recv() {
                let mut message = message.lock().unwrap();
                message.push_back(msg);
                sender.notice();
            }
        }));

        // handler ids equal or smaller than 0xFFFF are reserved by NWG
        let handler_id = 0x10000;
        let controller_clone = data.controller.clone();
        nwg::bind_raw_event_handler(&parent, handler_id, move |_hwnd, msg, _w, l| {
            if msg == WM_SETFOCUS {
                controller_clone.get().and_then(|controller| {
                    controller
                        .move_focus(webview2::MoveFocusReason::Programmatic)
                        .ok()
                });
            } else if msg == WM_APPCOMMAND {
                let cmd = ((l >> 16) & 0xFFF) as u32;
                let player_cmd = match cmd {
                    APPCOMMAND_MEDIA_PLAY_PAUSE
                    | APPCOMMAND_MEDIA_PLAY
                    | APPCOMMAND_MEDIA_PAUSE => Some(r#"["mpv-command", ["cycle", "pause"]]"#),
                    APPCOMMAND_MEDIA_NEXTTRACK => {
                        Some(r#"["mpv-command", ["seek", "10", "relative"]]"#)
                    }
                    APPCOMMAND_MEDIA_PREVIOUSTRACK => {
                        Some(r#"["mpv-command", ["seek", "-10", "relative"]]"#)
                    }
                    _ => None,
                };
                if let Some(player_cmd) = player_cmd {
                    if let Ok(guard) = PLAYER_CMD_TX.lock() {
                        if let Some(player_tx) = guard.as_ref() {
                            player_tx.send(player_cmd.to_string()).ok();
                        }
                    }
                    return Some(1);
                }
            }
            None
        })
        .ok();

        Ok(())
    }
    fn process_event<'a>(
        &self,
        evt: nwg::Event,
        _evt_data: &nwg::EventData,
        handle: nwg::ControlHandle,
    ) {
        use nwg::Event as E;
        if evt == E::OnNotice && handle == self.notice.handle {
            let message_queue = self.message_queue.clone();
            if let Some(controller) = self.controller.get() {
                let webview = controller.get_webview().expect("Cannot get vebview");
                let mut message_queue = message_queue.lock().unwrap();
                for msg in message_queue.drain(..) {
                    if let Some(script) = msg.strip_prefix(WEBVIEW_EXEC_SCRIPT_PREFIX) {
                        webview.execute_script(script, |_| Ok(())).ok();
                    } else {
                        webview.post_web_message_as_string(msg.as_str()).ok();
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::browser_reserved_mpv_key_with_modifiers;
    use winapi::um::winuser::VK_F7;

    #[test]
    fn browser_reserved_shortcuts_use_mpv_normalization() {
        assert_eq!(
            browser_reserved_mpv_key_with_modifiers(0x46, false, true, false, false),
            Some("Ctrl+f".to_string())
        );
        assert_eq!(
            browser_reserved_mpv_key_with_modifiers(0x46, true, true, false, false),
            Some("Ctrl+F".to_string())
        );
        assert_eq!(
            browser_reserved_mpv_key_with_modifiers(VK_F7 as u32, true, true, false, false,),
            Some("Shift+Ctrl+F7".to_string())
        );
    }
}
