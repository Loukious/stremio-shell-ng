use std::cell::RefCell;
use winapi::shared::minwindef::{BOOL, LPARAM, TRUE};
use winapi::shared::windef::HWND;
use winapi::um::winuser::{EnumChildWindows, GetClassNameA};

const CHROME_PREFIX: &str = "Chrome_";
const INTERMEDIATE_D3D: &str = "Intermediate D3D Window";

fn get_class_name(hwnd: HWND) -> String {
    let mut buf = [0i8; 256];
    let len = unsafe { GetClassNameA(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    if len <= 0 {
        return String::new();
    }
    let bytes: Vec<u8> = buf[..len as usize].iter().map(|c| *c as u8).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn is_webview_class(class: &str) -> bool {
    class.starts_with(CHROME_PREFIX) || class == INTERMEDIATE_D3D
}

struct EnumState {
    found: Option<HWND>,
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let state_cell = &*(lparam as *const RefCell<EnumState>);
    let class = get_class_name(hwnd);
    // Prefer an exact "mpv" class match; if MPV ever renames, fall through.
    if class.eq_ignore_ascii_case("mpv") {
        state_cell.borrow_mut().found = Some(hwnd);
        return 0; // stop enumeration
    }
    if !is_webview_class(&class) && state_cell.borrow().found.is_none() {
        state_cell.borrow_mut().found = Some(hwnd);
        // Keep enumerating to allow an explicit "mpv" match to override.
    }
    TRUE
}

pub fn find_mpv_child_hwnd(parent: HWND) -> Option<HWND> {
    let state = RefCell::new(EnumState { found: None });
    unsafe {
        EnumChildWindows(parent, Some(enum_proc), &state as *const _ as LPARAM);
    }
    state.into_inner().found
}
