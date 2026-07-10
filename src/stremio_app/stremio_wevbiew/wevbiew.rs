use crate::stremio_app::constants::SERVER_IPC_KEY;
use crate::stremio_app::ipc;
use crate::stremio_app::stremio_player::player::PLAYER_CMD_TX;
use native_windows_gui::{self as nwg, PartialUi};
use once_cell::sync::Lazy;
use once_cell::unsync::OnceCell;
use serde_json::json;
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::mem;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::thread;
use url::Url;
use urlencoding::decode;
use webview2::Controller;
use winapi::shared::windef::HWND;
use winapi::um::winuser::{GetClientRect, VK_F7, WM_APPCOMMAND, WM_SETFOCUS};

const APPCOMMAND_MEDIA_NEXTTRACK: u32 = 11;
const APPCOMMAND_MEDIA_PREVIOUSTRACK: u32 = 12;
const APPCOMMAND_MEDIA_PLAY_PAUSE: u32 = 14;
const APPCOMMAND_MEDIA_PLAY: u32 = 46;
const APPCOMMAND_MEDIA_PAUSE: u32 = 47;
pub static WEB_CMD_TX: Lazy<Mutex<Option<flume::Sender<String>>>> = Lazy::new(|| Mutex::new(None));
pub const WEBVIEW_EXEC_SCRIPT_PREFIX: &str = "__stremio_shell_exec_script__:";

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
                            Ok(())
                        }).expect("Cannot add content loading");

                        WebView::resize_to_window_bounds(Some(&controller), Some(hwnd));
                        controller.put_is_visible(true).ok();
                        controller
                            .move_focus(webview2::MoveFocusReason::Programmatic)
                            .ok();
                        controller.add_accelerator_key_pressed(move |_, e| {
                            // Block F7, Ctrl+F, and Ctrl+G
                            let k = e.get_virtual_key()?;
                            if k == VK_F7 as u32  || k == 70 & 0x7F || k == 71 & 0x7F {
                                e.put_handled(true)
                            } else {
                                Ok(())
                            }
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
