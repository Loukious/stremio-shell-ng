use native_windows_gui as nwg;
use serde_json::json;
use std::cell::Cell;
use std::ffi::OsStr;
use std::mem;
use std::os::windows::ffi::OsStrExt;
use std::ptr;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::thread;
use winapi::shared::minwindef::{LPARAM, LRESULT, UINT, WPARAM};
use winapi::shared::windef::{HBRUSH, HDC, HWND, POINT, RECT};
use winapi::um::libloaderapi::GetModuleHandleW;
use winapi::um::wingdi::{
    CombineRgn, CreateRectRgn, CreateRoundRectRgn, GetStockObject, BLACK_BRUSH, RGN_OR,
};
use winapi::um::winuser::{
    BeginPaint, ClientToScreen, CreateWindowExW, DefWindowProcW, EndPaint, FillRect, GetClientRect,
    GetCursorPos, GetMonitorInfoW, GetWindowLongA, GetWindowLongPtrW, GetWindowRect,
    InvalidateRect, IsWindowVisible, LoadCursorW, MonitorFromRect, MoveWindow, PostMessageW,
    RedrawWindow, RegisterClassExW, ReleaseCapture, SetCapture, SetLayeredWindowAttributes,
    SetParent, SetWindowLongA, SetWindowLongPtrW, SetWindowPos, SetWindowRgn, ShowWindow,
    GWLP_USERDATA, GWL_EXSTYLE, GWL_STYLE, HTBOTTOM, HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCAPTION,
    HTLEFT, HTRIGHT, HTTOP, HTTOPLEFT, HTTOPRIGHT, HWND_BOTTOM, HWND_TOP, HWND_TOPMOST, IDC_ARROW,
    LWA_ALPHA, MONITORINFO, MONITOR_DEFAULTTONEAREST, PAINTSTRUCT, RDW_INVALIDATE, RDW_UPDATENOW,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SW_HIDE, SW_SHOWNA,
    WM_DESTROY, WM_ERASEBKGND, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCCALCSIZE,
    WM_NCHITTEST, WM_NCLBUTTONDOWN, WM_PAINT, WM_SIZE, WNDCLASSEXW, WS_CAPTION, WS_CHILD,
    WS_CLIPSIBLINGS, WS_EX_LAYERED, WS_EX_TOPMOST, WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_SYSMENU,
    WS_THICKFRAME,
};

// -----------------------------------------------------------------------------
// GDI+ flat-API bindings (inline; winapi 0.3.9 does not ship these definitions).
// -----------------------------------------------------------------------------
#[allow(non_snake_case)]
mod gdip {
    use std::os::raw::c_void;

    pub type GpStatus = i32;
    pub type GpGraphics = c_void;
    pub type GpBrush = c_void;
    pub type GpPath = c_void;
    pub type GpFontFamily = c_void;
    pub type GpFont = c_void;
    pub type GpStringFormat = c_void;
    pub type Argb = u32;

    #[repr(C)]
    pub struct RectF {
        pub x: f32,
        pub y: f32,
        pub width: f32,
        pub height: f32,
    }

    // SmoothingMode
    pub const SMOOTHING_MODE_ANTI_ALIAS: i32 = 4;
    // TextRenderingHint
    pub const TEXT_RENDERING_HINT_ANTI_ALIAS_GRID_FIT: i32 = 3;
    // FillMode
    pub const FILL_MODE_ALTERNATE: i32 = 0;
    // StringAlignment
    pub const STRING_ALIGNMENT_CENTER: i32 = 1;
    // Unit
    pub const UNIT_PIXEL: i32 = 2;
    // FontStyle
    pub const FONT_STYLE_REGULAR: i32 = 0;

    #[link(name = "gdiplus")]
    extern "system" {
        pub fn GdipCreateFromHDC(hdc: super::HDC, graphics: *mut *mut GpGraphics) -> GpStatus;
        pub fn GdipDeleteGraphics(g: *mut GpGraphics) -> GpStatus;
        pub fn GdipSetSmoothingMode(g: *mut GpGraphics, mode: i32) -> GpStatus;
        pub fn GdipSetTextRenderingHint(g: *mut GpGraphics, hint: i32) -> GpStatus;
        pub fn GdipCreateSolidFill(color: Argb, brush: *mut *mut GpBrush) -> GpStatus;
        pub fn GdipDeleteBrush(b: *mut GpBrush) -> GpStatus;
        pub fn GdipCreatePath(fill_mode: i32, path: *mut *mut GpPath) -> GpStatus;
        pub fn GdipDeletePath(p: *mut GpPath) -> GpStatus;
        pub fn GdipAddPathArc(
            p: *mut GpPath,
            x: f32,
            y: f32,
            w: f32,
            h: f32,
            start: f32,
            sweep: f32,
        ) -> GpStatus;
        pub fn GdipClosePathFigure(p: *mut GpPath) -> GpStatus;
        pub fn GdipFillPath(g: *mut GpGraphics, brush: *mut GpBrush, path: *mut GpPath)
            -> GpStatus;
        pub fn GdipFillEllipse(
            g: *mut GpGraphics,
            brush: *mut GpBrush,
            x: f32,
            y: f32,
            w: f32,
            h: f32,
        ) -> GpStatus;
        pub fn GdipCreateFontFamilyFromName(
            name: *const u16,
            font_collection: *mut c_void,
            family: *mut *mut GpFontFamily,
        ) -> GpStatus;
        pub fn GdipDeleteFontFamily(f: *mut GpFontFamily) -> GpStatus;
        pub fn GdipCreateFont(
            family: *mut GpFontFamily,
            em_size: f32,
            style: i32,
            unit: i32,
            font: *mut *mut GpFont,
        ) -> GpStatus;
        pub fn GdipDeleteFont(f: *mut GpFont) -> GpStatus;
        pub fn GdipCreateStringFormat(
            attrs: i32,
            lang: u16,
            sf: *mut *mut GpStringFormat,
        ) -> GpStatus;
        pub fn GdipDeleteStringFormat(sf: *mut GpStringFormat) -> GpStatus;
        pub fn GdipSetStringFormatAlign(sf: *mut GpStringFormat, a: i32) -> GpStatus;
        pub fn GdipSetStringFormatLineAlign(sf: *mut GpStringFormat, a: i32) -> GpStatus;
        pub fn GdipDrawString(
            g: *mut GpGraphics,
            text: *const u16,
            length: i32,
            font: *mut GpFont,
            layout: *const RectF,
            format: *mut GpStringFormat,
            brush: *mut GpBrush,
        ) -> GpStatus;
    }

    /// Helper: pack Argb into a single u32 in the format GDI+ expects.
    #[inline]
    pub fn argb(a: u8, r: u8, g: u8, b: u8) -> Argb {
        ((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
    }
}

// -----------------------------------------------------------------------------
// Layout constants (overlay child coordinate space = PiP client area).
// -----------------------------------------------------------------------------
const PIP_DEFAULT_WIDTH: i32 = 520;
const PIP_DEFAULT_HEIGHT: i32 = 320;
const PIP_MIN_WIDTH: i32 = 280;
const PIP_MIN_HEIGHT: i32 = 160;
const RESIZE_BORDER: i32 = 8;
const PIP_RAW_HANDLER_ID: usize = 0x20000;
// Hover poll interval; the overlay shows on hover and hides this long after the
// cursor leaves the window.
const HOVER_POLL_MS: u32 = 180;
const HOVER_HIDE_AFTER_MS: u128 = 2200;

const BTN_SIZE: i32 = 40;
const BTN_MARGIN: i32 = 12;
const BTN_RADIUS: i32 = 12;
const SLIDER_H: i32 = 6;
const SLIDER_THUMB_R: i32 = 7;
const SLIDER_HIT_PAD: i32 = 10;
const DRAG_HANDLE_W: i32 = 96;
const DRAG_HANDLE_H: i32 = 10;

const TRANSPARENT_ALPHA: u8 = 150;
const PIP_SCREEN_MARGIN: i32 = 24;

// Hit identifiers for buttons.
#[repr(u8)]
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum HitId {
    None = 0,
    Restore,
    Close,
    PlayPause,
    Skip,
    Transparency,
    Slider,
}

fn sanitize_pip_bounds(
    initial_pos: Option<(i32, i32)>,
    initial_size: (i32, i32),
) -> (i32, i32, i32, i32) {
    let mut width = initial_size.0.max(PIP_MIN_WIDTH);
    let mut height = initial_size.1.max(PIP_MIN_HEIGHT);
    let desired_x = initial_pos.map(|(x, _)| x).unwrap_or(0);
    let desired_y = initial_pos.map(|(_, y)| y).unwrap_or(0);
    let desired = RECT {
        left: desired_x,
        top: desired_y,
        right: desired_x + width,
        bottom: desired_y + height,
    };

    let Some(work) = monitor_work_rect(&desired) else {
        return (desired_x, desired_y, width, height);
    };

    let work_width = (work.right - work.left).max(1);
    let work_height = (work.bottom - work.top).max(1);
    width = width.min(work_width).max(PIP_MIN_WIDTH.min(work_width));
    height = height.min(work_height).max(PIP_MIN_HEIGHT.min(work_height));

    let max_x = work.right - width;
    let max_y = work.bottom - height;
    let (x, y) = if let Some((x, y)) = initial_pos {
        (
            clamp_to_range(x, work.left, max_x),
            clamp_to_range(y, work.top, max_y),
        )
    } else {
        (
            clamp_to_range(work.right - width - PIP_SCREEN_MARGIN, work.left, max_x),
            clamp_to_range(work.bottom - height - PIP_SCREEN_MARGIN, work.top, max_y),
        )
    };

    if initial_pos.is_some_and(|pos| pos != (x, y)) {
        eprintln!(
            "PiP placement adjusted onto visible monitor: ({desired_x}, {desired_y}) -> ({x}, {y})"
        );
    }

    (x, y, width, height)
}

fn ensure_pip_window_visible(hwnd: HWND, reason: &str) {
    unsafe {
        let mut rect: RECT = mem::zeroed();
        if GetWindowRect(hwnd, &mut rect) == 0 {
            return;
        }

        let Some(work) = monitor_work_rect(&rect) else {
            return;
        };

        let work_width = (work.right - work.left).max(1);
        let work_height = (work.bottom - work.top).max(1);
        let width = (rect.right - rect.left).max(PIP_MIN_WIDTH).min(work_width);
        let height = (rect.bottom - rect.top)
            .max(PIP_MIN_HEIGHT)
            .min(work_height);
        let x = clamp_to_range(rect.left, work.left, work.right - width);
        let y = clamp_to_range(rect.top, work.top, work.bottom - height);

        if x == rect.left
            && y == rect.top
            && width == rect.right - rect.left
            && height == rect.bottom - rect.top
        {
            return;
        }

        eprintln!(
            "PiP window rect adjusted onto visible monitor ({reason}): ({}, {}, {}, {}) -> ({x}, {y}, {width}, {height})",
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top
        );
        SetWindowPos(hwnd, HWND_TOPMOST, x, y, width, height, SWP_NOACTIVATE);
    }
}

fn monitor_work_rect(rect: &RECT) -> Option<RECT> {
    unsafe {
        let monitor = MonitorFromRect(rect, MONITOR_DEFAULTTONEAREST);
        if monitor.is_null() {
            return None;
        }

        let mut info: MONITORINFO = mem::zeroed();
        info.cbSize = mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(monitor, &mut info) == 0 {
            return None;
        }
        Some(info.rcWork)
    }
}

fn clamp_to_range(value: i32, min: i32, max: i32) -> i32 {
    if max <= min {
        min
    } else {
        value.clamp(min, max)
    }
}

// -----------------------------------------------------------------------------
// Public NWG-managed wrapper.
// -----------------------------------------------------------------------------
// `nwg::Timer` is deprecated in favour of `AnimationTimer`, but that requires an
// extra NWG feature and a background animation thread. A plain interval timer is
// the right tool for a ~180 ms hover/redraw poll, so we keep it intentionally.
#[allow(deprecated)]
#[derive(Default)]
pub struct PipWindow {
    pub window: nwg::Window,
    pub hover_timer: nwg::Timer,

    pub mpv_child: Rc<Cell<Option<HWND>>>,
    pub overlay_hwnd: Cell<Option<HWND>>,
    pub built: Cell<bool>,

    pub time_pos: Arc<Mutex<f64>>,
    pub duration: Arc<Mutex<f64>>,
    pub is_paused: Arc<Mutex<bool>>,

    pub last_cursor_activity_ms: Rc<Cell<u128>>,
    pub overlay_visible: Rc<Cell<bool>>,
    pub transparent: Rc<Cell<bool>>,
}

pub struct PipBuildContext {
    pub close_sender: nwg::NoticeSender,
    pub player_tx: flume::Sender<String>,
    pub player_event_rx: flume::Receiver<String>,
    pub initial_pos: Option<(i32, i32)>,
    pub initial_size: Option<(i32, i32)>,
    pub initial_transparent: bool,
}

// -----------------------------------------------------------------------------
// State stored behind GWLP_USERDATA on the overlay child window.
// -----------------------------------------------------------------------------
struct PipOverlayState {
    player_tx: flume::Sender<String>,
    close_sender: nwg::NoticeSender,
    time_pos: Arc<Mutex<f64>>,
    duration: Arc<Mutex<f64>>,
    is_paused: Arc<Mutex<bool>>,
    pip_top_hwnd: HWND,
    transparent: Rc<Cell<bool>>,
    dragging_slider: Cell<bool>,
    hovered: Cell<HitId>,
}

// -----------------------------------------------------------------------------
// PipWindow impl.
// -----------------------------------------------------------------------------
impl PipWindow {
    #[allow(deprecated)] // nwg::Timer — see note on the PipWindow struct.
    pub fn build(&mut self, ctx: PipBuildContext) -> Result<(), nwg::NwgError> {
        if self.built.get() {
            return Ok(());
        }

        let (w, h) = ctx
            .initial_size
            .unwrap_or((PIP_DEFAULT_WIDTH, PIP_DEFAULT_HEIGHT));
        let (x, y, w, h) = sanitize_pip_bounds(ctx.initial_pos, (w, h));

        let window_builder = nwg::Window::builder()
            .title("Stremio - Picture in Picture")
            .size((w, h))
            .position((x, y))
            .flags(nwg::WindowFlags::WINDOW | nwg::WindowFlags::RESIZABLE);
        window_builder.build(&mut self.window)?;

        nwg::Timer::builder()
            .parent(&self.window)
            .interval(HOVER_POLL_MS)
            .stopped(false)
            .build(&mut self.hover_timer)?;

        // Strip caption/sysmenu/min-max; keep WS_THICKFRAME for invisible resize.
        let top_hwnd = self
            .window
            .handle
            .hwnd()
            .expect("PiP window has no HWND after build");
        unsafe {
            let style = GetWindowLongA(top_hwnd, GWL_STYLE);
            let new_style = (style
                & !(WS_CAPTION as i32
                    | WS_SYSMENU as i32
                    | WS_MINIMIZEBOX as i32
                    | WS_MAXIMIZEBOX as i32))
                | WS_THICKFRAME as i32;
            SetWindowLongA(top_hwnd, GWL_STYLE, new_style);

            let ex = GetWindowLongA(top_hwnd, GWL_EXSTYLE);
            SetWindowLongA(top_hwnd, GWL_EXSTYLE, ex | WS_EX_TOPMOST as i32);
            SetWindowPos(
                top_hwnd,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_FRAMECHANGED,
            );
        }
        ensure_pip_window_visible(top_hwnd, "after-create");

        // Apply initial transparency if remembered.
        self.transparent.set(ctx.initial_transparent);
        apply_transparency(top_hwnd, ctx.initial_transparent);

        // Register the overlay window class once.
        let class_name = wide_zero_terminated("StremioPipOverlay");
        unsafe {
            let hinstance = GetModuleHandleW(ptr::null());
            let wc = WNDCLASSEXW {
                cbSize: mem::size_of::<WNDCLASSEXW>() as u32,
                style: 0,
                lpfnWndProc: Some(overlay_wndproc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: hinstance,
                hIcon: ptr::null_mut(),
                hCursor: LoadCursorW(ptr::null_mut(), IDC_ARROW),
                hbrBackground: ptr::null_mut(),
                lpszMenuName: ptr::null(),
                lpszClassName: class_name.as_ptr(),
                hIconSm: ptr::null_mut(),
            };
            // Ignore failure if the class already exists.
            let _ = RegisterClassExW(&wc);
        }

        // Build the overlay state and stash it in GWLP_USERDATA.
        let overlay_state = Box::new(PipOverlayState {
            player_tx: ctx.player_tx.clone(),
            close_sender: ctx.close_sender,
            time_pos: Arc::clone(&self.time_pos),
            duration: Arc::clone(&self.duration),
            is_paused: Arc::clone(&self.is_paused),
            pip_top_hwnd: top_hwnd,
            transparent: Rc::clone(&self.transparent),
            dragging_slider: Cell::new(false),
            hovered: Cell::new(HitId::None),
        });
        let state_ptr = Box::into_raw(overlay_state);

        // Create the overlay child sized to client area, hidden initially.
        // Layered so we can give it a uniform alpha and the underlying MPV
        // video shows through.
        let overlay_hwnd = unsafe {
            let mut client: RECT = mem::zeroed();
            GetClientRect(top_hwnd, &mut client);
            let cw = client.right - client.left;
            let ch = client.bottom - client.top;
            let hwnd = CreateWindowExW(
                0,
                class_name.as_ptr(),
                ptr::null(),
                WS_CHILD | WS_CLIPSIBLINGS,
                0,
                0,
                cw,
                ch,
                top_hwnd,
                ptr::null_mut(),
                GetModuleHandleW(ptr::null()),
                ptr::null_mut(),
            );
            if hwnd.is_null() {
                drop(Box::from_raw(state_ptr));
                panic!("Failed to create PiP overlay window");
            }
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize);
            apply_pills_region(hwnd, cw, ch);
            hwnd
        };
        self.overlay_hwnd.set(Some(overlay_hwnd));

        // Hidden by default — only shown when the user hovers over PiP.
        unsafe {
            ShowWindow(overlay_hwnd, SW_HIDE);
        }

        // Observe MPV properties so the slider can track playback.
        let _ = ctx
            .player_tx
            .send(json!(["mpv-observe-prop", "time-pos"]).to_string());
        let _ = ctx
            .player_tx
            .send(json!(["mpv-observe-prop", "duration"]).to_string());
        let _ = ctx
            .player_tx
            .send(json!(["mpv-observe-prop", "pause"]).to_string());

        // Worker thread keeps the latest MPV time/duration/pause in shared state.
        // The overlay is repainted from the hover timer (below) rather than per
        // event, so we never paint-storm during playback.
        let time_pos = Arc::clone(&self.time_pos);
        let duration = Arc::clone(&self.duration);
        let is_paused = Arc::clone(&self.is_paused);
        let event_rx = ctx.player_event_rx;
        thread::spawn(move || {
            for msg in event_rx.iter() {
                let Ok(value) = serde_json::from_str::<serde_json::Value>(&msg) else {
                    continue;
                };
                let Some(args) = value.get("args").and_then(|v| v.as_array()) else {
                    continue;
                };
                if args.first().and_then(|v| v.as_str()) != Some("mpv-prop-change") {
                    continue;
                }
                let Some(inner) = args.get(1) else { continue };
                let name = inner.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let data = inner.get("data");
                match name {
                    "time-pos" => {
                        if let Some(t) = data.and_then(|v| v.as_f64()) {
                            *time_pos.lock().unwrap() = t;
                        }
                    }
                    "duration" => {
                        if let Some(d) = data.and_then(|v| v.as_f64()) {
                            *duration.lock().unwrap() = d;
                        }
                    }
                    "pause" => {
                        if let Some(p) = data.and_then(|v| v.as_bool()) {
                            *is_paused.lock().unwrap() = p;
                        }
                    }
                    _ => {}
                }
            }
        });

        // Timer drives the hover show/hide of the controls overlay. The overlay
        // is repainted when it is revealed (and on resize), reflecting the
        // playback position captured by the worker thread above.
        let window_handle = self.window.handle;
        let timer_handle = self.hover_timer.handle;
        let overlay_for_evt = overlay_hwnd;
        let overlay_visible_evt = Rc::clone(&self.overlay_visible);
        let last_cursor_clone = Rc::clone(&self.last_cursor_activity_ms);

        self.overlay_visible.set(false);
        self.last_cursor_activity_ms.set(now_ms());

        let _ = nwg::full_bind_event_handler(&window_handle, move |evt, _data, handle| {
            if evt != nwg::Event::OnTimerTick || handle != timer_handle {
                return;
            }
            let Some(pip_hwnd) = window_handle.hwnd() else {
                return;
            };
            if unsafe { IsWindowVisible(pip_hwnd) } == 0 {
                return;
            }
            if is_cursor_over_window(pip_hwnd) {
                last_cursor_clone.set(now_ms());
                if !overlay_visible_evt.get() {
                    // Reveal the controls and repaint them with the current
                    // playback position. They then auto-hide once the cursor
                    // has been away for HOVER_HIDE_AFTER_MS.
                    unsafe {
                        ShowWindow(overlay_for_evt, SW_SHOWNA);
                        InvalidateRect(overlay_for_evt, ptr::null(), 0);
                    }
                    overlay_visible_evt.set(true);
                }
            } else if overlay_visible_evt.get()
                && now_ms().saturating_sub(last_cursor_clone.get()) > HOVER_HIDE_AFTER_MS
            {
                unsafe { ShowWindow(overlay_for_evt, SW_HIDE) };
                overlay_visible_evt.set(false);
            }
        });

        // Raw handler on the PiP top-level: NCCALCSIZE for borderless,
        // NCHITTEST for resize zones, WM_SIZE to resize MPV + overlay.
        let mpv_child = Rc::clone(&self.mpv_child);
        let overlay_for_size = overlay_hwnd;
        nwg::bind_raw_event_handler(
            &window_handle,
            PIP_RAW_HANDLER_ID,
            move |hwnd, msg, w, l| match msg {
                WM_NCCALCSIZE => {
                    // wparam = TRUE means client size requested. Returning 0 in all
                    // edges expands the client area to cover the full window rect.
                    if w == 1 {
                        return Some(0);
                    }
                    None
                }
                WM_NCHITTEST => Some(hit_test(hwnd, l)),
                WM_SIZE => {
                    unsafe {
                        let mut rect: RECT = mem::zeroed();
                        if GetClientRect(hwnd, &mut rect) != 0 {
                            let cw = rect.right - rect.left;
                            let ch = rect.bottom - rect.top;
                            if let Some(child) = mpv_child.get() {
                                MoveWindow(child, 0, 0, cw, ch, 1);
                            }
                            MoveWindow(overlay_for_size, 0, 0, cw, ch, 1);
                            InvalidateRect(overlay_for_size, ptr::null(), 0);
                        }
                    }
                    None
                }
                _ => None,
            },
        )
        .ok();

        self.built.set(true);
        self.hide();
        Ok(())
    }

    pub fn hwnd(&self) -> Option<HWND> {
        self.window.handle.hwnd()
    }

    pub fn show(&self) {
        self.last_cursor_activity_ms.set(now_ms());
        self.window.set_visible(true);
        if let Some(overlay) = self.overlay_hwnd.get() {
            unsafe {
                ShowWindow(overlay, SW_SHOWNA);
                InvalidateRect(overlay, ptr::null(), 0);
            }
            self.overlay_visible.set(true);
        }
        if let Some(hwnd) = self.hwnd() {
            apply_transparency(hwnd, self.transparent.get());
            ensure_pip_window_visible(hwnd, "before-show");
            unsafe {
                SetWindowPos(
                    hwnd,
                    HWND_TOPMOST,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
                );
            }
        }
    }

    pub fn hide(&self) {
        self.window.set_visible(false);
    }

    pub fn attach_video(&self, child: HWND) {
        if let Some(target) = self.hwnd() {
            unsafe {
                SetParent(child, target);
                // Add WS_CLIPSIBLINGS WHILE MPV lives in the PiP window so its
                // continuous video painting is clipped out of the overlay's
                // (small, pill-shaped) rectangle and the controls stay visible.
                // detach_video() CLEARS this bit again before MPV returns to the
                // main window — there the WebView2 sibling is full-size, and a
                // clipped MPV would paint nowhere and show white.
                let style = GetWindowLongA(child, GWL_STYLE);
                SetWindowLongA(child, GWL_STYLE, style | WS_CLIPSIBLINGS as i32);

                let mut rect: RECT = mem::zeroed();
                if GetClientRect(target, &mut rect) != 0 {
                    MoveWindow(
                        child,
                        0,
                        0,
                        rect.right - rect.left,
                        rect.bottom - rect.top,
                        1,
                    );
                }
                SetWindowPos(
                    child,
                    HWND_BOTTOM,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED,
                );

                if let Some(overlay) = self.overlay_hwnd.get() {
                    let s = GetWindowLongA(overlay, GWL_STYLE);
                    SetWindowLongA(overlay, GWL_STYLE, s | WS_CLIPSIBLINGS as i32);
                    SetWindowPos(
                        overlay,
                        HWND_TOP,
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                    );
                    InvalidateRect(overlay, ptr::null(), 1);
                }
            }
            self.mpv_child.set(Some(child));
        }
    }

    pub fn detach_video(&self, target: HWND) {
        if let Some(child) = self.mpv_child.take() {
            unsafe {
                SetParent(child, target);
                // Ensure MPV does NOT carry WS_CLIPSIBLINGS in the main window,
                // otherwise it clips against the WebView2 sibling and renders
                // white. Clear the bit defensively.
                let style = GetWindowLongA(child, GWL_STYLE);
                SetWindowLongA(child, GWL_STYLE, style & !(WS_CLIPSIBLINGS as i32));

                let mut rect: RECT = mem::zeroed();
                if GetClientRect(target, &mut rect) != 0 {
                    let w = rect.right - rect.left;
                    let h = rect.bottom - rect.top;
                    MoveWindow(child, 0, 0, w, h, 1);
                }
                SetWindowPos(
                    child,
                    HWND_BOTTOM,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED,
                );
            }
        }
    }

    pub fn current_placement(&self) -> Option<(i32, i32, i32, i32)> {
        let hwnd = self.hwnd()?;
        unsafe {
            let mut rect: RECT = mem::zeroed();
            if GetWindowRect(hwnd, &mut rect) == 0 {
                return None;
            }
            Some((
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
            ))
        }
    }

    pub fn transparency_enabled(&self) -> bool {
        self.transparent.get()
    }
}

// -----------------------------------------------------------------------------
// Overlay child window procedure.
// -----------------------------------------------------------------------------
unsafe extern "system" fn overlay_wndproc(
    hwnd: HWND,
    msg: UINT,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut PipOverlayState;

    match msg {
        WM_ERASEBKGND => return 1,
        WM_PAINT => {
            if !state_ptr.is_null() {
                paint_overlay(hwnd, &*state_ptr);
            } else {
                let mut ps: PAINTSTRUCT = mem::zeroed();
                let _hdc = BeginPaint(hwnd, &mut ps);
                EndPaint(hwnd, &ps);
            }
            return 0;
        }
        WM_LBUTTONDOWN => {
            if state_ptr.is_null() {
                return 0;
            }
            let state = &*state_ptr;
            let x = (lparam & 0xFFFF) as i16 as i32;
            let y = ((lparam >> 16) & 0xFFFF) as i16 as i32;
            let layout = compute_layout(hwnd);
            let hit = hit_test_buttons(&layout, x, y);
            match hit {
                HitId::Restore | HitId::Close => {
                    state.close_sender.notice();
                }
                HitId::PlayPause => {
                    let _ = state
                        .player_tx
                        .send(json!(["mpv-command", ["cycle", "pause"]]).to_string());
                }
                HitId::Skip => {
                    let t = *state.time_pos.lock().unwrap() + 30.0;
                    let _ = state
                        .player_tx
                        .send(json!(["mpv-set-prop", ["time-pos", t]]).to_string());
                }
                HitId::Transparency => {
                    let new_val = !state.transparent.get();
                    state.transparent.set(new_val);
                    apply_transparency(state.pip_top_hwnd, new_val);
                    InvalidateRect(hwnd, ptr::null(), 0);
                }
                HitId::Slider => {
                    state.dragging_slider.set(true);
                    SetCapture(hwnd);
                    seek_from_slider_x(state, &layout, x, hwnd);
                }
                HitId::None => {
                    // Drag the PiP window when clicking empty overlay area.
                    // Convert overlay-client coords to screen coords for WM_NCLBUTTONDOWN.
                    let mut pt = POINT { x, y };
                    ClientToScreen(hwnd, &mut pt);
                    ReleaseCapture();
                    let lp_screen = (pt.x & 0xFFFF) | ((pt.y & 0xFFFF) << 16);
                    PostMessageW(
                        state.pip_top_hwnd,
                        WM_NCLBUTTONDOWN,
                        HTCAPTION as WPARAM,
                        lp_screen as LPARAM,
                    );
                }
            }
            return 0;
        }
        WM_MOUSEMOVE => {
            if state_ptr.is_null() {
                return 0;
            }
            let state = &*state_ptr;
            let x = (lparam & 0xFFFF) as i16 as i32;
            let y = ((lparam >> 16) & 0xFFFF) as i16 as i32;
            let layout = compute_layout(hwnd);
            if state.dragging_slider.get() {
                seek_from_slider_x(state, &layout, x, hwnd);
                return 0;
            }
            let hit = hit_test_buttons(&layout, x, y);
            if state.hovered.get() != hit {
                state.hovered.set(hit);
                InvalidateRect(hwnd, ptr::null(), 0);
            }
            return 0;
        }
        WM_LBUTTONUP => {
            if state_ptr.is_null() {
                return 0;
            }
            let state = &*state_ptr;
            if state.dragging_slider.get() {
                state.dragging_slider.set(false);
                ReleaseCapture();
            }
            return 0;
        }
        WM_SIZE => {
            let cw = (lparam & 0xFFFF) as i32;
            let ch = ((lparam >> 16) & 0xFFFF) as i32;
            // Update the window region to the new size, then force a SYNCHRONOUS
            // full repaint so the painted pills and the (just-changed) region
            // are always in lockstep. Without this, live-resize leaves stale
            // glass-fill smears where the region exists but the paint lagged.
            apply_pills_region(hwnd, cw, ch);
            InvalidateRect(hwnd, ptr::null(), 1);
            RedrawWindow(
                hwnd,
                ptr::null(),
                ptr::null_mut(),
                RDW_INVALIDATE | RDW_UPDATENOW,
            );
            return 0;
        }
        WM_DESTROY => {
            if !state_ptr.is_null() {
                let _ = Box::from_raw(state_ptr);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
            return 0;
        }
        _ => {}
    }

    DefWindowProcW(hwnd, msg, wparam, lparam)
}

// -----------------------------------------------------------------------------
// Layout computation.
// -----------------------------------------------------------------------------
struct Layout {
    restore: RECT,
    close: RECT,
    play_pause: RECT,
    skip: RECT,
    transparency: RECT,
    slider_track: RECT,
    drag_handle: RECT,
}

unsafe fn compute_layout(hwnd: HWND) -> Layout {
    let mut client: RECT = mem::zeroed();
    GetClientRect(hwnd, &mut client);
    layout_from_size(client.right - client.left, client.bottom - client.top)
}

fn layout_from_size(width: i32, height: i32) -> Layout {
    let top_y = BTN_MARGIN;
    let bottom_y = height - BTN_MARGIN - BTN_SIZE;

    let restore = RECT {
        left: BTN_MARGIN,
        top: top_y,
        right: BTN_MARGIN + BTN_SIZE,
        bottom: top_y + BTN_SIZE,
    };
    let close = RECT {
        left: width - BTN_MARGIN - BTN_SIZE,
        top: top_y,
        right: width - BTN_MARGIN,
        bottom: top_y + BTN_SIZE,
    };

    let play_pause = RECT {
        left: BTN_MARGIN,
        top: bottom_y,
        right: BTN_MARGIN + BTN_SIZE,
        bottom: bottom_y + BTN_SIZE,
    };
    let skip = RECT {
        left: BTN_MARGIN + BTN_SIZE + 8,
        top: bottom_y,
        right: BTN_MARGIN + 2 * BTN_SIZE + 8,
        bottom: bottom_y + BTN_SIZE,
    };
    let transparency = RECT {
        left: width - BTN_MARGIN - BTN_SIZE,
        top: bottom_y,
        right: width - BTN_MARGIN,
        bottom: bottom_y + BTN_SIZE,
    };

    let slider_y = bottom_y - 18;
    let slider_track = RECT {
        left: BTN_MARGIN,
        top: slider_y,
        right: width - BTN_MARGIN,
        bottom: slider_y + SLIDER_H,
    };

    // Small "grip" pill centered between the top buttons that the user can
    // grab to drag the PiP window. Lives between restore and close so it can
    // never overlap them.
    let drag_x = (width - DRAG_HANDLE_W) / 2;
    let drag_y = BTN_MARGIN + (BTN_SIZE - DRAG_HANDLE_H) / 2;
    let drag_handle = RECT {
        left: drag_x.max(restore.right + 8),
        top: drag_y,
        right: (drag_x + DRAG_HANDLE_W).min(close.left - 8),
        bottom: drag_y + DRAG_HANDLE_H,
    };

    Layout {
        restore,
        close,
        play_pause,
        skip,
        transparency,
        slider_track,
        drag_handle,
    }
}

fn rect_contains(r: &RECT, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

fn inflate(r: &RECT, by: i32) -> RECT {
    RECT {
        left: r.left - by,
        top: r.top - by,
        right: r.right + by,
        bottom: r.bottom + by,
    }
}

fn slider_hit_rect(track: &RECT) -> RECT {
    // Generous vertical hit area so the slim slider is easier to grab.
    RECT {
        left: track.left,
        top: track.top - 10,
        right: track.right,
        bottom: track.bottom + 10,
    }
}

fn hit_test_buttons(l: &Layout, x: i32, y: i32) -> HitId {
    if rect_contains(&l.restore, x, y) {
        return HitId::Restore;
    }
    if rect_contains(&l.close, x, y) {
        return HitId::Close;
    }
    if rect_contains(&l.play_pause, x, y) {
        return HitId::PlayPause;
    }
    if rect_contains(&l.skip, x, y) {
        return HitId::Skip;
    }
    if rect_contains(&l.transparency, x, y) {
        return HitId::Transparency;
    }
    if rect_contains(&slider_hit_rect(&l.slider_track), x, y) {
        return HitId::Slider;
    }
    HitId::None
}

fn seek_from_slider_x(state: &PipOverlayState, layout: &Layout, x: i32, overlay_hwnd: HWND) {
    let track = &layout.slider_track;
    let w = (track.right - track.left).max(1);
    let clamped = (x - track.left).clamp(0, w);
    let frac = clamped as f64 / w as f64;
    let dur = *state.duration.lock().unwrap();
    if dur > 0.0 {
        let target = (frac * dur).max(0.0);
        *state.time_pos.lock().unwrap() = target;
        // Only repaint the overlay (NOT the whole PiP top-level / MPV).
        unsafe { InvalidateRect(overlay_hwnd, ptr::null(), 0) };
        let _ = state
            .player_tx
            .send(json!(["mpv-set-prop", ["time-pos", target]]).to_string());
    }
}

// -----------------------------------------------------------------------------
// Painting (GDI+).
// -----------------------------------------------------------------------------
unsafe fn paint_overlay(hwnd: HWND, state: &PipOverlayState) {
    let mut ps: PAINTSTRUCT = mem::zeroed();
    let hdc = BeginPaint(hwnd, &mut ps);

    // The overlay window is not layered, so its framebuffer persists between
    // paints and we suppress WM_ERASEBKGND. Clear the invalidated area to
    // opaque black first; otherwise the translucent glass fills blend with the
    // previous frame and the moving slider thumb leaves a ghost trail.
    FillRect(
        hdc,
        &ps.rcPaint,
        GetStockObject(BLACK_BRUSH as i32) as HBRUSH,
    );

    let mut g: *mut gdip::GpGraphics = ptr::null_mut();
    if gdip::GdipCreateFromHDC(hdc, &mut g) != 0 || g.is_null() {
        EndPaint(hwnd, &ps);
        return;
    }
    gdip::GdipSetSmoothingMode(g, gdip::SMOOTHING_MODE_ANTI_ALIAS);
    gdip::GdipSetTextRenderingHint(g, gdip::TEXT_RENDERING_HINT_ANTI_ALIAS_GRID_FIT);

    let layout = compute_layout(hwnd);
    let hovered = state.hovered.get();
    // No full-overlay scrim — the window region clips us down to just the
    // individual pill rects, so MPV is visible everywhere else.

    // Glass pill colors: translucent dark for fill, lighter on hover, white text.
    let bg_color = gdip::argb(180, 14, 14, 18);
    let bg_color_hover = gdip::argb(210, 30, 30, 36);
    let active_accent = gdip::argb(220, 123, 91, 245); // Stremio purple

    let text_white = gdip::argb(255, 255, 255, 255);
    let slider_track_color = gdip::argb(110, 220, 220, 220);
    let slider_fill_color = gdip::argb(245, 123, 91, 245);
    let slider_thumb_color = gdip::argb(255, 255, 255, 255);

    let mut brush_bg: *mut gdip::GpBrush = ptr::null_mut();
    gdip::GdipCreateSolidFill(bg_color, &mut brush_bg);
    let mut brush_bg_hover: *mut gdip::GpBrush = ptr::null_mut();
    gdip::GdipCreateSolidFill(bg_color_hover, &mut brush_bg_hover);
    let mut brush_text: *mut gdip::GpBrush = ptr::null_mut();
    gdip::GdipCreateSolidFill(text_white, &mut brush_text);
    let mut brush_track: *mut gdip::GpBrush = ptr::null_mut();
    gdip::GdipCreateSolidFill(slider_track_color, &mut brush_track);
    let mut brush_fill: *mut gdip::GpBrush = ptr::null_mut();
    gdip::GdipCreateSolidFill(slider_fill_color, &mut brush_fill);
    let mut brush_thumb: *mut gdip::GpBrush = ptr::null_mut();
    gdip::GdipCreateSolidFill(slider_thumb_color, &mut brush_thumb);
    let mut brush_accent: *mut gdip::GpBrush = ptr::null_mut();
    gdip::GdipCreateSolidFill(active_accent, &mut brush_accent);

    // Font: Segoe UI Symbol (has the glyphs we use).
    let family_name = wide_zero_terminated("Segoe UI Symbol");
    let mut family: *mut gdip::GpFontFamily = ptr::null_mut();
    if gdip::GdipCreateFontFamilyFromName(family_name.as_ptr(), ptr::null_mut(), &mut family) != 0 {
        // Fall back to Segoe UI.
        let fallback = wide_zero_terminated("Segoe UI");
        gdip::GdipCreateFontFamilyFromName(fallback.as_ptr(), ptr::null_mut(), &mut family);
    }
    let mut glyph_font: *mut gdip::GpFont = ptr::null_mut();
    gdip::GdipCreateFont(
        family,
        18.0,
        gdip::FONT_STYLE_REGULAR,
        gdip::UNIT_PIXEL,
        &mut glyph_font,
    );

    let mut center_fmt: *mut gdip::GpStringFormat = ptr::null_mut();
    gdip::GdipCreateStringFormat(0, 0, &mut center_fmt);
    gdip::GdipSetStringFormatAlign(center_fmt, gdip::STRING_ALIGNMENT_CENTER);
    gdip::GdipSetStringFormatLineAlign(center_fmt, gdip::STRING_ALIGNMENT_CENTER);

    let draw_pill = |rect: &RECT, this_hit: HitId, glyph: &str, accent: bool| {
        let mut path: *mut gdip::GpPath = ptr::null_mut();
        gdip::GdipCreatePath(gdip::FILL_MODE_ALTERNATE, &mut path);
        // Overfill the glass background by 1px so its anti-aliased edge lands
        // OUTSIDE the hard-edged window region (which clips it cleanly). This
        // prevents a 1px stale rim around each pill.
        rounded_rect_path(path, &inflate(rect, 1), BTN_RADIUS + 1);
        let fill = if accent {
            brush_accent
        } else if hovered == this_hit {
            brush_bg_hover
        } else {
            brush_bg
        };
        gdip::GdipFillPath(g, fill, path);
        gdip::GdipDeletePath(path);

        // Glyph.
        let wide = wide_zero_terminated(glyph);
        let layout = gdip::RectF {
            x: rect.left as f32,
            y: rect.top as f32,
            width: (rect.right - rect.left) as f32,
            height: (rect.bottom - rect.top) as f32,
        };
        gdip::GdipDrawString(
            g,
            wide.as_ptr(),
            (wide.len() - 1) as i32,
            glyph_font,
            &layout,
            center_fmt,
            brush_text,
        );
    };

    draw_pill(&layout.restore, HitId::Restore, "\u{21A9}", false);
    draw_pill(&layout.close, HitId::Close, "\u{2715}", false);

    // Drag handle: small horizontal "grip" pill, no glyph, just a flat fill
    // so it reads as an affordance for moving the window.
    {
        let mut path: *mut gdip::GpPath = ptr::null_mut();
        gdip::GdipCreatePath(gdip::FILL_MODE_ALTERNATE, &mut path);
        rounded_rect_path(
            path,
            &inflate(&layout.drag_handle, 1),
            DRAG_HANDLE_H / 2 + 1,
        );
        gdip::GdipFillPath(g, brush_bg, path);
        gdip::GdipDeletePath(path);
    }

    let paused = *state.is_paused.lock().unwrap();
    let pp_glyph = if paused { "\u{25B6}" } else { "\u{23F8}" };
    draw_pill(&layout.play_pause, HitId::PlayPause, pp_glyph, false);
    draw_pill(&layout.skip, HitId::Skip, "\u{23ED}", false);

    let transparent = state.transparent.get();
    draw_pill(
        &layout.transparency,
        HitId::Transparency,
        "\u{25D0}",
        transparent,
    );

    // Slider — paint the FULL hit region as a glass container first so the
    // window region (which is the padded hit area) has no unpainted band
    // (that band would otherwise show stale framebuffer pixels => artifacts).
    {
        let track = layout.slider_track;
        let container = slider_hit_rect(&track);

        // Glass container background, filling the entire slider region
        // (overfilled by 1px so the AA edge is clipped by the region).
        let mut path: *mut gdip::GpPath = ptr::null_mut();
        gdip::GdipCreatePath(gdip::FILL_MODE_ALTERNATE, &mut path);
        let container_radius = (container.bottom - container.top) / 2 + 1;
        rounded_rect_path(path, &inflate(&container, 1), container_radius);
        gdip::GdipFillPath(g, brush_bg, path);
        gdip::GdipDeletePath(path);

        // Track (unfilled portion), centered vertically in the container.
        let mut path: *mut gdip::GpPath = ptr::null_mut();
        gdip::GdipCreatePath(gdip::FILL_MODE_ALTERNATE, &mut path);
        rounded_rect_path(path, &track, SLIDER_H / 2);
        gdip::GdipFillPath(g, brush_track, path);
        gdip::GdipDeletePath(path);

        let t = *state.time_pos.lock().unwrap();
        let d = *state.duration.lock().unwrap();
        let frac = if d > 0.0 {
            (t / d).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let track_w = (track.right - track.left) as f32;
        let fill_w = (track_w * frac as f32).max(0.0);
        if fill_w > 0.1 {
            let fill_rect = RECT {
                left: track.left,
                top: track.top,
                right: track.left + fill_w as i32,
                bottom: track.bottom,
            };
            let mut path: *mut gdip::GpPath = ptr::null_mut();
            gdip::GdipCreatePath(gdip::FILL_MODE_ALTERNATE, &mut path);
            rounded_rect_path(path, &fill_rect, SLIDER_H / 2);
            gdip::GdipFillPath(g, brush_fill, path);
            gdip::GdipDeletePath(path);
        }

        // Thumb.
        let thumb_cx = track.left as f32 + fill_w;
        let thumb_cy = ((track.top + track.bottom) as f32) / 2.0;
        gdip::GdipFillEllipse(
            g,
            brush_thumb,
            thumb_cx - SLIDER_THUMB_R as f32,
            thumb_cy - SLIDER_THUMB_R as f32,
            (SLIDER_THUMB_R * 2) as f32,
            (SLIDER_THUMB_R * 2) as f32,
        );
    }

    // Cleanup.
    gdip::GdipDeleteStringFormat(center_fmt);
    gdip::GdipDeleteFont(glyph_font);
    gdip::GdipDeleteFontFamily(family);
    gdip::GdipDeleteBrush(brush_bg);
    gdip::GdipDeleteBrush(brush_bg_hover);
    gdip::GdipDeleteBrush(brush_text);
    gdip::GdipDeleteBrush(brush_track);
    gdip::GdipDeleteBrush(brush_fill);
    gdip::GdipDeleteBrush(brush_thumb);
    gdip::GdipDeleteBrush(brush_accent);
    gdip::GdipDeleteGraphics(g);

    EndPaint(hwnd, &ps);
}

unsafe fn rounded_rect_path(path: *mut gdip::GpPath, r: &RECT, radius: i32) {
    let x = r.left as f32;
    let y = r.top as f32;
    let w = (r.right - r.left) as f32;
    let h = (r.bottom - r.top) as f32;
    let d = (radius * 2) as f32;
    gdip::GdipAddPathArc(path, x, y, d, d, 180.0, 90.0);
    gdip::GdipAddPathArc(path, x + w - d, y, d, d, 270.0, 90.0);
    gdip::GdipAddPathArc(path, x + w - d, y + h - d, d, d, 0.0, 90.0);
    gdip::GdipAddPathArc(path, x, y + h - d, d, d, 90.0, 90.0);
    gdip::GdipClosePathFigure(path);
}

// -----------------------------------------------------------------------------
// Hit testing for the PiP top-level (drag + resize). Same as before.
// -----------------------------------------------------------------------------
fn hit_test(hwnd: HWND, lparam: LPARAM) -> LRESULT {
    unsafe {
        let mut rect: RECT = mem::zeroed();
        if GetWindowRect(hwnd, &mut rect) == 0 {
            return HTCAPTION as LRESULT;
        }
        let cursor = POINT {
            x: (lparam & 0xFFFF) as i16 as i32,
            y: ((lparam >> 16) & 0xFFFF) as i16 as i32,
        };
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        if width < PIP_MIN_WIDTH || height < PIP_MIN_HEIGHT {
            return HTCAPTION as LRESULT;
        }
        let on_left = cursor.x - rect.left < RESIZE_BORDER;
        let on_right = rect.right - cursor.x < RESIZE_BORDER;
        let on_top = cursor.y - rect.top < RESIZE_BORDER;
        let on_bottom = rect.bottom - cursor.y < RESIZE_BORDER;
        let zone = match (on_left, on_right, on_top, on_bottom) {
            (true, _, true, _) => HTTOPLEFT,
            (_, true, true, _) => HTTOPRIGHT,
            (true, _, _, true) => HTBOTTOMLEFT,
            (_, true, _, true) => HTBOTTOMRIGHT,
            (true, _, _, _) => HTLEFT,
            (_, true, _, _) => HTRIGHT,
            (_, _, true, _) => HTTOP,
            (_, _, _, true) => HTBOTTOM,
            _ => HTCAPTION,
        };
        zone as LRESULT
    }
}

/// Replace the overlay's window region with a UNION of the individual pill
/// rects (buttons + slider hit area + drag handle). The corners, gaps between
/// pills, and the middle of the PiP are all OUTSIDE the region, so resize
/// (HTBOTTOMRIGHT etc) on the PiP top-level works again, and MPV is fully
/// visible everywhere except where a pill sits.
unsafe fn apply_pills_region(hwnd: HWND, w: i32, h: i32) {
    let layout = layout_from_size(w, h);
    let pills = [
        rounded_pill_rect(&layout.restore, BTN_RADIUS),
        rounded_pill_rect(&layout.close, BTN_RADIUS),
        rounded_pill_rect(&layout.play_pause, BTN_RADIUS),
        rounded_pill_rect(&layout.skip, BTN_RADIUS),
        rounded_pill_rect(&layout.transparency, BTN_RADIUS),
        rounded_pill_rect(
            &slider_hit_rect(&layout.slider_track),
            SLIDER_H + SLIDER_HIT_PAD,
        ),
        rounded_pill_rect(&layout.drag_handle, DRAG_HANDLE_H / 2),
    ];

    let combined = CreateRectRgn(0, 0, 0, 0);
    for hrgn in &pills {
        CombineRgn(combined, combined, *hrgn, RGN_OR);
        winapi::um::wingdi::DeleteObject(*hrgn as _);
    }
    // `combined` is owned by the window after SetWindowRgn — don't delete.
    SetWindowRgn(hwnd, combined, 1);
    let _ = w;
    let _ = h;
}

unsafe fn rounded_pill_rect(r: &RECT, radius: i32) -> winapi::shared::minwindef::HRGN {
    // CreateRoundRectRgn uses an exclusive bottom-right corner.
    let d = (radius * 2).max(2);
    CreateRoundRectRgn(r.left, r.top, r.right + 1, r.bottom + 1, d, d)
}

fn is_cursor_over_window(hwnd: HWND) -> bool {
    unsafe {
        let mut pt: POINT = mem::zeroed();
        if GetCursorPos(&mut pt) == 0 {
            return false;
        }
        let mut rect: RECT = mem::zeroed();
        if GetWindowRect(hwnd, &mut rect) == 0 {
            return false;
        }
        pt.x >= rect.left && pt.x < rect.right && pt.y >= rect.top && pt.y < rect.bottom
    }
}

// -----------------------------------------------------------------------------
// Transparency toggle (uniform alpha on the PiP top-level).
// -----------------------------------------------------------------------------
fn apply_transparency(hwnd: HWND, on: bool) {
    unsafe {
        let ex = GetWindowLongA(hwnd, GWL_EXSTYLE) as u32;
        if on {
            if (ex & WS_EX_LAYERED) == 0 {
                SetWindowLongA(hwnd, GWL_EXSTYLE, (ex | WS_EX_LAYERED) as i32);
            }
            SetLayeredWindowAttributes(hwnd, 0, TRANSPARENT_ALPHA, LWA_ALPHA);
        } else if (ex & WS_EX_LAYERED) != 0 {
            // Keep the layered bit but raise alpha to fully opaque — cheaper than
            // ripping the ex-style off and avoids a stutter on toggle.
            SetLayeredWindowAttributes(hwnd, 0, 255, LWA_ALPHA);
        }
    }
}

// -----------------------------------------------------------------------------
// Misc helpers.
// -----------------------------------------------------------------------------
fn now_ms() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn wide_zero_terminated(s: &str) -> Vec<u16> {
    OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}
