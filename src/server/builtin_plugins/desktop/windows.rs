//! Native Win32 + PowerShell 7 UI Automation backend for local Windows desktop control.
use super::*;
use std::{
    ffi::{OsStr, OsString},
    io::Cursor,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::PathBuf,
    process::Stdio,
    sync::Once,
    time::Duration,
};
use tokio::{io::AsyncReadExt, process::Command};
use windows_sys::Win32::{
    Foundation::{CloseHandle, GlobalFree, HWND, LPARAM, MAX_PATH, POINT, RECT},
    Graphics::{
        Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute},
        Gdi::{
            BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleBitmap,
            CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits,
            HDC, HGDIOBJ, ReleaseDC, SRCCOPY, SelectObject,
        },
    },
    Storage::Xps::PrintWindow,
    System::{
        DataExchange::{
            CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable,
            OpenClipboard, SetClipboardData,
        },
        Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
        Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW},
    },
    UI::{
        HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
        Input::KeyboardAndMouse::{
            INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
            KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN,
            MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE,
            MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK,
            MOUSEEVENTF_WHEEL, MOUSEINPUT, SendInput, VIRTUAL_KEY, VK_BACK, VK_CONTROL, VK_DELETE,
            VK_DOWN, VK_END, VK_ESCAPE, VK_F1, VK_HOME, VK_LEFT, VK_MENU, VK_NEXT, VK_PRIOR,
            VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP,
        },
        Shell::ShellExecuteW,
        WindowsAndMessaging::{
            BringWindowToTop, EnumWindows, GA_ROOT, GWL_EXSTYLE, GetAncestor, GetClassNameW,
            GetForegroundWindow, GetSystemMetrics, GetWindowLongW, GetWindowRect,
            GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindow,
            IsWindowVisible, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
            SM_YVIRTUALSCREEN, SW_RESTORE, SW_SHOWNORMAL, SetForegroundWindow, ShowWindow,
            WS_EX_TOOLWINDOW, WindowFromPoint,
        },
    },
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const CF_UNICODETEXT: u32 = 13;
const PW_RENDERFULLCONTENT: u32 = 0x0000_0002;
const MAX_IMAGE: usize = visual_artifacts::MAX_ORIGINAL_BYTES;
const MAX_TEXT: usize = 64 * 1024;
const MAX_UIA_OUTPUT: usize = 1024 * 1024;

#[derive(Default)]
pub(super) struct Windows;

fn ensure_dpi_aware() {
    static DPI_INIT: Once = Once::new();
    DPI_INIT.call_once(|| {
        // SAFETY: Process-wide DPI awareness setup; failure (e.g. already set) is harmless.
        unsafe {
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
    });
}

fn to_wide_null(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

fn trim_utf8(value: &str, max_bytes: usize) -> String {
    value[..value.floor_char_boundary(max_bytes)].to_owned()
}

pub(super) fn format_hwnd(hwnd: HWND) -> String {
    format!("hwnd-{:x}", hwnd as usize)
}

pub(super) fn parse_hwnd(id: &str) -> Option<HWND> {
    let hex = id.strip_prefix("hwnd-")?;
    let raw = usize::from_str_radix(hex, 16).ok()?;
    (raw != 0).then_some(raw as HWND)
}

fn window_title(hwnd: HWND) -> String {
    // SAFETY: `GetWindowTextLengthW` / `GetWindowTextW` operate on a valid window handle.
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len as usize + 1];
        let written = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
        if written <= 0 {
            return String::new();
        }
        trim_utf8(&String::from_utf16_lossy(&buf[..written as usize]), 240)
    }
}

fn window_class(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    // SAFETY: `buf` is a valid mutable UTF-16 buffer of length 256.
    let len = unsafe { GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    if len <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..len as usize])
}

fn process_basename(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    // SAFETY: `OpenProcess` with `PROCESS_QUERY_LIMITED_INFORMATION` is read-only.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let mut buf = vec![0u16; MAX_PATH as usize * 4];
    let mut size = buf.len() as u32;
    // SAFETY: `handle` is valid and closed immediately after query.
    let ok = unsafe { QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut size) };
    unsafe {
        CloseHandle(handle);
    }
    if ok == 0 || size == 0 {
        return None;
    }
    let full = OsString::from_wide(&buf[..size as usize]);
    PathBuf::from(full)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

fn window_app_class(hwnd: HWND, pid: Option<u32>) -> String {
    let class = window_class(hwnd);
    let exe = pid.and_then(process_basename);
    let label = match (exe, class.is_empty()) {
        (Some(exe), false) => format!("{exe} ({class})"),
        (Some(exe), true) => exe,
        (None, false) => class,
        (None, true) => String::new(),
    };
    trim_utf8(&label, 240)
}

fn extended_frame_rect(hwnd: HWND) -> Option<RECT> {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: `DwmGetWindowAttribute` writes a `RECT` of exact byte size.
    let hr = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS as u32,
            (&mut rect as *mut RECT).cast(),
            std::mem::size_of::<RECT>() as u32,
        )
    };
    if hr == 0 && rect.right > rect.left && rect.bottom > rect.top {
        return Some(rect);
    }
    // SAFETY: Fallback to standard `GetWindowRect`.
    let ok = unsafe { GetWindowRect(hwnd, &mut rect) };
    (ok != 0).then_some(rect)
}

fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked: u32 = 0;
    // SAFETY: `DwmGetWindowAttribute` writes a `u32` flag.
    let hr = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED as u32,
            (&mut cloaked as *mut u32).cast(),
            std::mem::size_of::<u32>() as u32,
        )
    };
    hr == 0 && cloaked != 0
}

fn window_from_hwnd(hwnd: HWND, foreground: HWND) -> Option<Window> {
    // SAFETY: Read-only Win32 window queries on `hwnd`.
    unsafe {
        if IsWindow(hwnd) == 0 || IsWindowVisible(hwnd) == 0 {
            return None;
        }
        if is_cloaked(hwnd) {
            return None;
        }
        let active = hwnd == foreground;
        let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if (ex_style & WS_EX_TOOLWINDOW) != 0 && !active {
            return None;
        }
        let iconic = IsIconic(hwnd) != 0;
        let title = window_title(hwnd);
        if title.is_empty() && !active {
            return None;
        }
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        let pid = (pid > 0).then_some(pid);
        let app_class = window_app_class(hwnd, pid);
        let (x, y, width, height) = if iconic {
            (0, 0, 1, 1)
        } else {
            let rect = extended_frame_rect(hwnd)?;
            let w = rect.right.saturating_sub(rect.left);
            let h = rect.bottom.saturating_sub(rect.top);
            if w <= 0 || h <= 0 {
                return None;
            }
            (rect.left, rect.top, w as u32, h as u32)
        };
        Some(Window {
            window_id: format_hwnd(hwnd),
            title,
            app_class,
            x,
            y,
            width,
            height,
            active,
            visible: !iconic,
            pid,
        })
    }
}

fn list_windows_sync() -> Vec<Window> {
    ensure_dpi_aware();
    struct State {
        foreground: HWND,
        out: Vec<Window>,
    }
    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> i32 {
        // SAFETY: `lparam` points to `State` on the caller's stack for the duration of `EnumWindows`.
        let state = unsafe { &mut *(lparam as *mut State) };
        if state.out.len() >= 200 {
            return 0;
        }
        if let Some(window) = window_from_hwnd(hwnd, state.foreground) {
            state.out.push(window);
        }
        1
    }
    // SAFETY: `GetForegroundWindow` and `EnumWindows` with valid stack pointer `state`.
    let mut state = State {
        foreground: unsafe { GetForegroundWindow() },
        out: Vec::new(),
    };
    unsafe {
        EnumWindows(Some(enum_proc), (&mut state as *mut State) as LPARAM);
    }
    state.out
}

fn verify_live_window(window: &Window, require_active: bool) -> Result<(HWND, Window)> {
    let hwnd = parse_hwnd(&window.window_id)
        .ok_or_else(|| fault("INVALID_ARGUMENT", "invalid Windows HWND identifier"))?;
    let live = guard::window(&list_windows_sync(), Some(&window.window_id), require_active)?;
    if live.pid != window.pid || live.app_class != window.app_class {
        return Err(fault("WINDOW_CHANGED", "window identity changed"));
    }
    if require_active && live != *window {
        return Err(fault(
            "WINDOW_CHANGED",
            "target focus or geometry changed; refresh desktop.list_windows",
        ));
    }
    Ok((hwnd, live))
}

fn verify_point_hits_window(hwnd: HWND, point: Point) -> Result<()> {
    // SAFETY: Read-only Win32 hit-test at screen coordinates.
    let hit = unsafe { WindowFromPoint(POINT { x: point.x, y: point.y }) };
    if hit.is_null() {
        return Err(fault(
            "WINDOW_CHANGED",
            "pointer is not over any window at the verified coordinates",
        ));
    }
    // SAFETY: `GetAncestor` resolves child controls to their top-level owner window.
    let root = unsafe { GetAncestor(hit, GA_ROOT) };
    if hit != hwnd && root != hwnd {
        return Err(fault(
            "WINDOW_CHANGED",
            "pointer is obscured by another window or overlay",
        ));
    }
    Ok(())
}

struct ScreenDc(HDC);
impl Drop for ScreenDc {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: Releases screen DC acquired via `GetDC(null)`.
            unsafe {
                ReleaseDC(std::ptr::null_mut(), self.0);
            }
        }
    }
}

struct MemDc(HDC);
impl Drop for MemDc {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: Deletes memory DC created via `CreateCompatibleDC`.
            unsafe {
                DeleteDC(self.0);
            }
        }
    }
}

struct GdiObj(HGDIOBJ);
impl Drop for GdiObj {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: Deletes GDI bitmap created via `CreateCompatibleBitmap`.
            unsafe {
                DeleteObject(self.0);
            }
        }
    }
}

fn dib_to_rgb_png(
    mem_dc: HDC,
    bitmap: HGDIOBJ,
    full_w: i32,
    full_h: i32,
    crop_x: i32,
    crop_y: i32,
    crop_w: u32,
    crop_h: u32,
) -> Result<(Vec<u8>, bool)> {
    let mut bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: full_w,
            biHeight: -full_h, // Top-down DIB
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        },
        bmiColors: [Default::default()],
    };
    let mut bgra = vec![0u8; (full_w as usize) * (full_h as usize) * 4];
    // SAFETY: `bgra` is sized to `full_w * full_h * 4` bytes matching 32bpp top-down DIB.
    let lines = unsafe {
        GetDIBits(
            mem_dc,
            bitmap,
            0,
            full_h as u32,
            bgra.as_mut_ptr().cast(),
            &mut bmi,
            DIB_RGB_COLORS,
        )
    };
    ensure!(lines != 0, "GetDIBits failed");

    let mut rgb = Vec::with_capacity((crop_w as usize) * (crop_h as usize) * 3);
    let mut non_black = false;
    for row in 0..crop_h as i32 {
        let sy = (crop_y + row).clamp(0, full_h - 1) as usize;
        for col in 0..crop_w as i32 {
            let sx = (crop_x + col).clamp(0, full_w - 1) as usize;
            let idx = (sy * (full_w as usize) + sx) * 4;
            let b = bgra[idx];
            let g = bgra[idx + 1];
            let r = bgra[idx + 2];
            if (r | g | b) != 0 {
                non_black = true;
            }
            rgb.push(r);
            rgb.push(g);
            rgb.push(b);
        }
    }
    let img = image::RgbImage::from_raw(crop_w, crop_h, rgb)
        .context("failed to build RGB image buffer")?;
    let mut png = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img).write_to(&mut png, image::ImageFormat::Png)?;
    let bytes = png.into_inner();
    if bytes.len() > MAX_IMAGE {
        return Err(fault(
            "DESKTOP_OUTPUT_LIMIT",
            "screenshot PNG exceeded maximum image size budget",
        ));
    }
    Ok((bytes, non_black))
}

fn capture_screen_rect_png(x: i32, y: i32, width: u32, height: u32) -> Result<Vec<u8>> {
    ensure!(width > 0 && height > 0, "empty screenshot region");
    // SAFETY: GDI DC and bitmap lifetime are managed by RAII drop guards.
    unsafe {
        let screen_dc = ScreenDc(GetDC(std::ptr::null_mut()));
        ensure!(!screen_dc.0.is_null(), "GetDC failed");
        let mem_dc = MemDc(CreateCompatibleDC(screen_dc.0));
        ensure!(!mem_dc.0.is_null(), "CreateCompatibleDC failed");
        let bmp = GdiObj(CreateCompatibleBitmap(
            screen_dc.0,
            width as i32,
            height as i32,
        ));
        ensure!(!bmp.0.is_null(), "CreateCompatibleBitmap failed");
        let prev = SelectObject(mem_dc.0, bmp.0);
        let ok = BitBlt(
            mem_dc.0,
            0,
            0,
            width as i32,
            height as i32,
            screen_dc.0,
            x,
            y,
            SRCCOPY | CAPTUREBLT,
        );
        SelectObject(mem_dc.0, prev);
        ensure!(ok != 0, "BitBlt screen capture failed");
        let (png, _) = dib_to_rgb_png(
            mem_dc.0,
            bmp.0,
            width as i32,
            height as i32,
            0,
            0,
            width,
            height,
        )?;
        Ok(png)
    }
}

fn capture_window_png(window: &Window) -> Result<Vec<u8>> {
    ensure_dpi_aware();
    let (hwnd, live) = verify_live_window(window, false)?;
    // SAFETY: `GetWindowRect` and `PrintWindow` operate on verified live `hwnd`.
    let printed = unsafe {
        let mut win_rect = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        if GetWindowRect(hwnd, &mut win_rect) != 0 {
            let full_w = win_rect.right.saturating_sub(win_rect.left);
            let full_h = win_rect.bottom.saturating_sub(win_rect.top);
            if full_w > 0 && full_h > 0 {
                let screen_dc = ScreenDc(GetDC(std::ptr::null_mut()));
                if !screen_dc.0.is_null() {
                    let mem_dc = MemDc(CreateCompatibleDC(screen_dc.0));
                    if !mem_dc.0.is_null() {
                        let bmp = GdiObj(CreateCompatibleBitmap(screen_dc.0, full_w, full_h));
                        if !bmp.0.is_null() {
                            let prev = SelectObject(mem_dc.0, bmp.0);
                            let ok = PrintWindow(hwnd, mem_dc.0, PW_RENDERFULLCONTENT);
                            SelectObject(mem_dc.0, prev);
                            if ok != 0 {
                                let crop_x = (live.x - win_rect.left).max(0);
                                let crop_y = (live.y - win_rect.top).max(0);
                                let crop_w = live.width.min((full_w - crop_x).max(1) as u32);
                                let crop_h = live.height.min((full_h - crop_y).max(1) as u32);
                                if let Ok((png, non_black)) = dib_to_rgb_png(
                                    mem_dc.0, bmp.0, full_w, full_h, crop_x, crop_y, crop_w, crop_h,
                                ) && non_black
                                {
                                    Some(png)
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        }
    };
    if let Some(png) = printed {
        return Ok(png);
    }
    capture_screen_rect_png(live.x, live.y, live.width, live.height)
}

fn capture_full_desktop_png() -> Result<Vec<u8>> {
    ensure_dpi_aware();
    // SAFETY: `GetSystemMetrics` reads virtual screen dimensions.
    let (x, y, w, h) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    ensure!(w > 0 && h > 0, "virtual desktop geometry unavailable");
    capture_screen_rect_png(x, y, w as u32, h as u32)
}

struct ClipboardGuard;
impl ClipboardGuard {
    fn open() -> Result<Self> {
        for attempt in 0..5 {
            // SAFETY: Opens the system clipboard for the current thread.
            if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
                return Ok(Self);
            }
            if attempt < 4 {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        Err(fault(
            "DESKTOP_BACKEND_FAILED",
            "could not open Windows clipboard (busy in another process)",
        ))
    }
}
impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        // SAFETY: Balances a successful `OpenClipboard`.
        unsafe {
            CloseClipboard();
        }
    }
}

fn clipboard_read_sync() -> Result<String> {
    let _guard = ClipboardGuard::open()?;
    // SAFETY: Clipboard is open for the duration of `_guard`.
    unsafe {
        if IsClipboardFormatAvailable(CF_UNICODETEXT) == 0 {
            return Ok(String::new());
        }
        let handle = GetClipboardData(CF_UNICODETEXT);
        if handle.is_null() {
            return Ok(String::new());
        }
        let ptr = GlobalLock(handle) as *const u16;
        if ptr.is_null() {
            return Err(fault("DESKTOP_BACKEND_FAILED", "GlobalLock clipboard failed"));
        }
        let mut len = 0usize;
        let max_u16 = MAX_TEXT + 1;
        while len < max_u16 && *ptr.add(len) != 0 {
            len += 1;
        }
        let slice = std::slice::from_raw_parts(ptr, len);
        let text = String::from_utf16_lossy(slice);
        GlobalUnlock(handle);
        if text.len() > MAX_TEXT {
            return Err(fault(
                "DESKTOP_OUTPUT_LIMIT",
                "clipboard text exceeded maximum budget",
            ));
        }
        Ok(text)
    }
}

fn clipboard_write_sync(text: &str) -> Result<()> {
    let wide = to_wide_null(text);
    let byte_len = wide.len() * std::mem::size_of::<u16>();
    let _guard = ClipboardGuard::open()?;
    // SAFETY: Allocates moveable global memory and hands ownership to `SetClipboardData`.
    unsafe {
        if EmptyClipboard() == 0 {
            return Err(fault("DESKTOP_BACKEND_FAILED", "EmptyClipboard failed"));
        }
        let mem = GlobalAlloc(GMEM_MOVEABLE, byte_len);
        if mem.is_null() {
            return Err(fault("DESKTOP_BACKEND_FAILED", "GlobalAlloc failed"));
        }
        let dst = GlobalLock(mem) as *mut u16;
        if dst.is_null() {
            GlobalFree(mem);
            return Err(fault("DESKTOP_BACKEND_FAILED", "GlobalLock failed"));
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), dst, wide.len());
        GlobalUnlock(mem);
        if SetClipboardData(CF_UNICODETEXT, mem).is_null() {
            GlobalFree(mem);
            return Err(fault("DESKTOP_BACKEND_FAILED", "SetClipboardData failed"));
        }
    }
    Ok(())
}

pub(super) fn normalize_virtual_coord(val: i32, origin: i32, span: i32) -> i32 {
    let denom = i64::from((span - 1).max(1));
    let rel = (i64::from(val) - i64::from(origin)).clamp(0, denom);
    ((rel * 65_535 + denom / 2) / denom) as i32
}

fn virtual_mouse_move(point: Point) -> INPUT {
    // SAFETY: `GetSystemMetrics` reads virtual screen bounds.
    let (vx, vy, vw, vh) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: normalize_virtual_coord(point.x, vx, vw),
                dy: normalize_virtual_coord(point.y, vy, vh),
                mouseData: 0,
                dwFlags: MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn mouse_flag_input(flags: u32, mouse_data: u32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: mouse_data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send_inputs(inputs: &[INPUT]) -> Result<()> {
    if inputs.is_empty() {
        return Ok(());
    }
    // SAFETY: `inputs` is a valid contiguous slice of Win32 `INPUT` structs.
    let sent = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        )
    };
    if sent as usize != inputs.len() {
        return Err(fault(
            "DESKTOP_BACKEND_FAILED",
            "SendInput was blocked (target window may be elevated/UIPI-protected)",
        ));
    }
    Ok(())
}

struct ReleaseLeftButton(bool);
impl Drop for ReleaseLeftButton {
    fn drop(&mut self) {
        if self.0 {
            let _ = send_inputs(&[mouse_flag_input(MOUSEEVENTF_LEFTUP, 0)]);
        }
    }
}

pub(super) fn map_virtual_key(key: &str) -> Option<(VIRTUAL_KEY, bool)> {
    match key {
        "Delete" => Some((VK_DELETE, true)),
        "Return" => Some((VK_RETURN, false)),
        "Escape" => Some((VK_ESCAPE, false)),
        "Tab" => Some((VK_TAB, false)),
        "BackSpace" => Some((VK_BACK, false)),
        "space" => Some((VK_SPACE, false)),
        "Left" => Some((VK_LEFT, true)),
        "Right" => Some((VK_RIGHT, true)),
        "Up" => Some((VK_UP, true)),
        "Down" => Some((VK_DOWN, true)),
        "Home" => Some((VK_HOME, true)),
        "End" => Some((VK_END, true)),
        "Page_Up" => Some((VK_PRIOR, true)),
        "Page_Down" => Some((VK_NEXT, true)),
        "F1" => Some((VK_F1, false)),
        "F2" => Some((VK_F1 + 1, false)),
        "F3" => Some((VK_F1 + 2, false)),
        "F4" => Some((VK_F1 + 3, false)),
        "F5" => Some((VK_F1 + 4, false)),
        "F6" => Some((VK_F1 + 5, false)),
        "F7" => Some((VK_F1 + 6, false)),
        "F8" => Some((VK_F1 + 7, false)),
        "F9" => Some((VK_F1 + 8, false)),
        "F10" => Some((VK_F1 + 9, false)),
        "F11" => Some((VK_F1 + 10, false)),
        "F12" => Some((VK_F1 + 11, false)),
        s if s.len() == 1 && s.as_bytes()[0].is_ascii_alphanumeric() => {
            Some((s.as_bytes()[0].to_ascii_uppercase() as VIRTUAL_KEY, false))
        }
        _ => None,
    }
}

fn modifier_vk(modifier: &str) -> Option<VIRTUAL_KEY> {
    match modifier {
        "ctrl" => Some(VK_CONTROL),
        "alt" => Some(VK_MENU),
        "shift" => Some(VK_SHIFT),
        _ => None,
    }
}

fn vk_input(vk: VIRTUAL_KEY, extended: bool, key_up: bool) -> INPUT {
    let mut flags = 0u32;
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if key_up {
        flags |= KEYEVENTF_KEYUP;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn unicode_input(code_unit: u16, key_up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: 0,
                wScan: code_unit,
                dwFlags: KEYEVENTF_UNICODE | if key_up { KEYEVENTF_KEYUP } else { 0 },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

pub(super) fn resolve_powershell() -> PathBuf {
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let pwsh = dir.join("pwsh.exe");
            if pwsh.is_file() {
                return pwsh;
            }
        }
    }
    for env_key in ["ProgramFiles", "ProgramW6432"] {
        if let Some(root) = std::env::var_os(env_key) {
            let pwsh = PathBuf::from(root).join("PowerShell/7/pwsh.exe");
            if pwsh.is_file() {
                return pwsh;
            }
        }
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let pwsh = PathBuf::from(local).join("Microsoft/WindowsApps/pwsh.exe");
        if pwsh.is_file() {
            return pwsh;
        }
    }
    let sys_root = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    sys_root.join("System32/WindowsPowerShell/v1.0/powershell.exe")
}

async fn run_powershell(
    script: &str,
    envs: &[(&str, String)],
    timeout: Duration,
    max_bytes: usize,
) -> Result<Vec<u8>> {
    let exe = resolve_powershell();
    let mut cmd = Command::new(&exe);
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    cmd.creation_flags(CREATE_NO_WINDOW);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().map_err(|error| {
        fault(
            "DESKTOP_BACKEND_FAILED",
            format!("could not launch PowerShell ({}): {error}", exe.display()),
        )
    })?;
    let stdout = child
        .stdout
        .take()
        .context("PowerShell stdout is unavailable")?;
    let stderr = child
        .stderr
        .take()
        .context("PowerShell stderr is unavailable")?;
    tokio::time::timeout(timeout, async {
        let mut out_bytes = Vec::new();
        let mut err_bytes = Vec::new();
        let mut out_reader = stdout.take(max_bytes as u64 + 1);
        let mut err_reader = stderr.take(4096);
        let (r1, r2) = tokio::join!(
            out_reader.read_to_end(&mut out_bytes),
            err_reader.read_to_end(&mut err_bytes)
        );
        r1?;
        r2?;
        if out_bytes.len() > max_bytes {
            return Err(fault(
                "DESKTOP_OUTPUT_LIMIT",
                "PowerShell desktop helper exceeded its output budget",
            ));
        }
        let status = child.wait().await?;
        if !status.success() {
            let err_msg = String::from_utf8_lossy(&err_bytes).trim().to_owned();
            return Err(fault(
                "DESKTOP_BACKEND_FAILED",
                if err_msg.is_empty() {
                    format!("PowerShell desktop helper exited with {status}")
                } else {
                    format!("PowerShell desktop helper failed: {}", trim_utf8(&err_msg, 240))
                },
            ));
        }
        Ok(out_bytes)
    })
    .await
    .map_err(|_| fault("DESKTOP_TIMEOUT", "PowerShell desktop helper timed out"))?
}

fn map_windows_app(app: &str) -> Option<&'static str> {
    match app {
        "calc" | "org.gnome.Calculator.desktop" | "org.kde.kcalc.desktop" => Some("calc.exe"),
        "notepad" | "org.gnome.TextEditor.desktop" | "org.kde.kate.desktop" => Some("notepad.exe"),
        "mspaint" => Some("mspaint.exe"),
        "msedge" => Some("msedge.exe"),
        "chrome" | "chromium.desktop" => Some("chrome.exe"),
        "firefox" | "firefox.desktop" => Some("firefox.exe"),
        "explorer" => Some("explorer.exe"),
        "wt" => Some("wt.exe"),
        _ => None,
    }
}

fn shell_execute_open(target: &str) -> Result<()> {
    let verb = to_wide_null("open");
    let file = to_wide_null(target);
    // SAFETY: Null-terminated UTF-16 strings passed to `ShellExecuteW`.
    let rc = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    } as isize;
    if rc <= 32 {
        return Err(fault(
            "DESKTOP_BACKEND_FAILED",
            format!("ShellExecuteW failed with code {rc} for target"),
        ));
    }
    Ok(())
}

const NOTIFY_SCRIPT: &str = r#"
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
$icon = New-Object System.Windows.Forms.NotifyIcon
$icon.Icon = [System.Drawing.SystemIcons]::Information
$icon.BalloonTipTitle = $env:QA_DESKTOP_TITLE
$icon.BalloonTipText = $env:QA_DESKTOP_BODY
$icon.Visible = $true
$icon.ShowBalloonTip(3000)
Start-Sleep -Milliseconds 300
$icon.Dispose()
"#;

const UIA_READ_TREE_SCRIPT: &str = r#"
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
$hwnd = [IntPtr]::new([Int64]$env:QA_DESKTOP_HWND)
$limit = [Int32]$env:QA_DESKTOP_LIMIT
$root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
if ($null -eq $root) { throw 'AutomationElement.FromHandle returned null' }
$walker = [System.Windows.Automation.TreeWalker]::ControlViewWalker
$queue = [System.Collections.Generic.Queue[object]]::new()
$queue.Enqueue(@($root, 0))
$nodes = [System.Collections.Generic.List[object]]::new()
while ($queue.Count -gt 0 -and $nodes.Count -lt $limit) {
  $pair = $queue.Dequeue()
  $el = $pair[0]
  $depth = [Int32]$pair[1]
  try {
    $cur = $el.Current
    $name = if ($null -ne $cur.Name) { [string]$cur.Name } else { '' }
    $role = if ($null -ne $cur.ControlType) { [string]$cur.ControlType.ProgrammaticName } else { '' }
    $role = $role -replace '^ControlType\.', ''
    $rid = ''
    try {
      $arr = $el.GetRuntimeId()
      if ($null -ne $arr -and $arr.Length -gt 0) { $rid = ($arr -join ',') }
    } catch {}
    $invokable = $false
    if (-not $cur.IsPassword -and $cur.IsEnabled) {
      foreach ($pat in @([System.Windows.Automation.InvokePattern]::Pattern, [System.Windows.Automation.TogglePattern]::Pattern, [System.Windows.Automation.SelectionItemPattern]::Pattern)) {
        try {
          if ($el.GetCurrentPropertyValue([System.Windows.Automation.AutomationElement]::IsControlElementProperty)) {
            $tmp = $null
            if ($el.TryGetCurrentPattern($pat, [ref]$tmp)) { $invokable = $true; break }
          }
        } catch {}
      }
    }
    $nodes.Add([PSCustomObject]@{
      name = $name
      role = $role
      depth = $depth
      runtime_id = $rid
      invokable = $invokable
    })
    if ($depth -lt 10) {
      $child = $walker.GetFirstChild($el)
      $siblingCount = 0
      while ($null -ne $child -and $siblingCount -lt 64 -and ($nodes.Count + $queue.Count) -lt ($limit * 3)) {
        $queue.Enqueue(@($child, ($depth + 1)))
        $siblingCount++
        $child = $walker.GetNextSibling($child)
      }
    }
  } catch {}
}
ConvertTo-Json -InputObject @($nodes) -Depth 4 -Compress
"#;

const UIA_INVOKE_SCRIPT: &str = r#"
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
$hwnd = [IntPtr]::new([Int64]$env:QA_DESKTOP_HWND)
$targetRid = [string]$env:QA_DESKTOP_RUNTIME_ID
$root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
if ($null -eq $root) { throw 'STALE_REF: window UIA root unavailable' }
$walker = [System.Windows.Automation.TreeWalker]::ControlViewWalker
$queue = [System.Collections.Generic.Queue[object]]::new()
$queue.Enqueue(@($root, 0))
$found = $null
$visited = 0
while ($queue.Count -gt 0 -and $visited -lt 800) {
  $pair = $queue.Dequeue()
  $el = $pair[0]
  $depth = [Int32]$pair[1]
  $visited++
  try {
    $arr = $el.GetRuntimeId()
    if ($null -ne $arr -and ($arr -join ',') -eq $targetRid) {
      $found = $el
      break
    }
    if ($depth -lt 10) {
      $child = $walker.GetFirstChild($el)
      $siblingCount = 0
      while ($null -ne $child -and $siblingCount -lt 64) {
        $queue.Enqueue(@($child, ($depth + 1)))
        $siblingCount++
        $child = $walker.GetNextSibling($child)
      }
    }
  } catch {}
}
if ($null -eq $found) { throw 'STALE_REF: UIA control no longer exists' }
if ($found.Current.IsPassword) { throw 'SENSITIVE_WINDOW: password field cannot be invoked' }
$pat = $null
if ($found.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$pat)) {
  $pat.Invoke()
} elseif ($found.TryGetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern, [ref]$pat)) {
  $pat.Toggle()
} elseif ($found.TryGetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern, [ref]$pat)) {
  $pat.Select()
} else {
  throw 'STALE_REF: UIA control has no supported invoke pattern'
}
"#;

impl DesktopBackend for Windows {
    async fn windows(&self) -> Result<Vec<Window>> {
        Ok(tokio::task::spawn_blocking(list_windows_sync).await?)
    }

    async fn screenshot(&self, window: Option<&Window>) -> Result<Vec<u8>> {
        let target = window.cloned();
        tokio::task::spawn_blocking(move || match target {
            Some(win) => capture_window_png(&win),
            None => capture_full_desktop_png(),
        })
        .await?
    }

    async fn clipboard_read(&self) -> Result<String> {
        tokio::task::spawn_blocking(clipboard_read_sync).await?
    }

    async fn clipboard_write(&self, text: &str, cancel: &CancellationToken) -> Result<()> {
        guard::cancelled(cancel)?;
        let owned = text.to_owned();
        tokio::task::spawn_blocking(move || clipboard_write_sync(&owned)).await?
    }

    async fn focus_target(&self, id: &str) -> Result<Window> {
        let windows = self.windows().await?;
        guard::focus_window(&windows, id)
    }

    async fn focus(&self, window: &Window, cancel: &CancellationToken) -> Result<()> {
        guard::cancelled(cancel)?;
        let target = window.clone();
        tokio::task::spawn_blocking(move || {
            ensure_dpi_aware();
            let hwnd = parse_hwnd(&target.window_id)
                .ok_or_else(|| fault("INVALID_ARGUMENT", "invalid Windows HWND identifier"))?;
            let live = guard::focus_window(&list_windows_sync(), &target.window_id)?;
            if live.pid != target.pid || live.app_class != target.app_class {
                return Err(fault("WINDOW_CHANGED", "window identity changed"));
            }
            // SAFETY: Standard Win32 focus sequence for a verified top-level window.
            unsafe {
                if IsIconic(hwnd) != 0 {
                    ShowWindow(hwnd, SW_RESTORE);
                }
                BringWindowToTop(hwnd);
                if SetForegroundWindow(hwnd) == 0 && GetForegroundWindow() != hwnd {
                    return Err(fault(
                        "DESKTOP_BACKEND_FAILED",
                        "Windows refused SetForegroundWindow (target may be elevated or foreground-locked)",
                    ));
                }
            }
            Ok(())
        })
        .await?
    }

    async fn notify(&self, title: &str, body: &str, cancel: &CancellationToken) -> Result<()> {
        guard::cancelled(cancel)?;
        run_powershell(
            NOTIFY_SCRIPT,
            &[
                ("QA_DESKTOP_TITLE", title.to_owned()),
                ("QA_DESKTOP_BODY", body.to_owned()),
            ],
            Duration::from_secs(6),
            4096,
        )
        .await?;
        Ok(())
    }

    async fn open(&self, target: &guard::OpenTarget, cancel: &CancellationToken) -> Result<()> {
        guard::cancelled(cancel)?;
        let launch_target = match target {
            guard::OpenTarget::Url(url) => url.as_str().to_owned(),
            guard::OpenTarget::App(app) => map_windows_app(app)
                .ok_or_else(|| {
                    fault(
                        "INVALID_TARGET",
                        format!("application '{app}' is not mapped on Windows"),
                    )
                })?
                .to_owned(),
        };
        tokio::task::spawn_blocking(move || shell_execute_open(&launch_target)).await?
    }

    async fn mouse(
        &self,
        window: &Window,
        point: Point,
        action: Pointer,
        cancel: &CancellationToken,
    ) -> Result<()> {
        guard::cancelled(cancel)?;
        let win = window.clone();
        let cancel_clone = cancel.clone();
        tokio::task::spawn_blocking(move || {
            ensure_dpi_aware();
            let (hwnd, live) = verify_live_window(&win, true)?;
            let translate = |pt: Point| -> Result<Point> {
                let args = json!({
                    "x": i64::from(pt.x) - i64::from(win.x),
                    "y": i64::from(pt.y) - i64::from(win.y),
                });
                guard::point(&args, "x", "y", &live, true)
            };
            let start = translate(point)?;
            let drag_end = match action {
                Pointer::Drag(end) => Some(translate(end)?),
                _ => None,
            };
            verify_point_hits_window(hwnd, start)?;
            if let Some(end) = drag_end {
                verify_point_hits_window(hwnd, end)?;
            }
            guard::cancelled(&cancel_clone)?;
            match action {
                Pointer::Move => send_inputs(&[virtual_mouse_move(start)]),
                Pointer::Click(button) => {
                    let (down, up) = match button {
                        1 => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
                        2 => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
                        3 => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
                        _ => return Err(fault("INVALID_ARGUMENT", "unsupported mouse button")),
                    };
                    send_inputs(&[
                        virtual_mouse_move(start),
                        mouse_flag_input(down, 0),
                        mouse_flag_input(up, 0),
                    ])
                }
                Pointer::DoubleClick => {
                    send_inputs(&[
                        virtual_mouse_move(start),
                        mouse_flag_input(MOUSEEVENTF_LEFTDOWN, 0),
                        mouse_flag_input(MOUSEEVENTF_LEFTUP, 0),
                    ])?;
                    std::thread::sleep(Duration::from_millis(55));
                    guard::cancelled(&cancel_clone)?;
                    verify_point_hits_window(hwnd, start)?;
                    send_inputs(&[
                        virtual_mouse_move(start),
                        mouse_flag_input(MOUSEEVENTF_LEFTDOWN, 0),
                        mouse_flag_input(MOUSEEVENTF_LEFTUP, 0),
                    ])
                }
                Pointer::Scroll(delta) => {
                    // Positive delta_y scrolls down (negative Win32 WHEEL_DELTA).
                    let wheel_data = (-delta) as u32;
                    send_inputs(&[
                        virtual_mouse_move(start),
                        mouse_flag_input(MOUSEEVENTF_WHEEL, wheel_data),
                    ])
                }
                Pointer::Drag(_) => {
                    let end = drag_end.context("missing drag end")?;
                    let mut release = ReleaseLeftButton(true);
                    send_inputs(&[
                        virtual_mouse_move(start),
                        mouse_flag_input(MOUSEEVENTF_LEFTDOWN, 0),
                    ])?;
                    std::thread::sleep(Duration::from_millis(25));
                    guard::cancelled(&cancel_clone)?;
                    send_inputs(&[virtual_mouse_move(end)])?;
                    std::thread::sleep(Duration::from_millis(25));
                    send_inputs(&[mouse_flag_input(MOUSEEVENTF_LEFTUP, 0)])?;
                    release.0 = false;
                    Ok(())
                }
            }
        })
        .await?
    }

    async fn keyboard(
        &self,
        window: &Window,
        input: &Keyboard,
        cancel: &CancellationToken,
    ) -> Result<()> {
        guard::cancelled(cancel)?;
        let win = window.clone();
        let batch = match input {
            Keyboard::Text(text) => {
                let mut events = Vec::with_capacity(text.len() * 2);
                for code_unit in text.encode_utf16() {
                    events.push(unicode_input(code_unit, false));
                    events.push(unicode_input(code_unit, true));
                }
                events
            }
            Keyboard::Key { modifiers, key } => {
                let (vk, extended) = map_virtual_key(key)
                    .ok_or_else(|| fault("UNSAFE_KEY", "unsupported key on Windows"))?;
                let mut mod_vks = Vec::with_capacity(modifiers.len());
                for m in modifiers {
                    mod_vks.push(
                        modifier_vk(m)
                            .ok_or_else(|| fault("UNSAFE_KEY", "unsupported modifier"))?,
                    );
                }
                let mut events = Vec::with_capacity(mod_vks.len() * 2 + 2);
                for &mvk in &mod_vks {
                    events.push(vk_input(mvk, false, false));
                }
                events.push(vk_input(vk, extended, false));
                events.push(vk_input(vk, extended, true));
                for &mvk in mod_vks.iter().rev() {
                    events.push(vk_input(mvk, false, true));
                }
                events
            }
        };
        tokio::task::spawn_blocking(move || {
            ensure_dpi_aware();
            let _ = verify_live_window(&win, true)?;
            send_inputs(&batch)
        })
        .await?
    }

    async fn read_tree(&self, window: &Window, limit: usize) -> Result<Vec<SemanticNode>> {
        let win = window.clone();
        let (hwnd_raw, live) = tokio::task::spawn_blocking(move || {
            let (hwnd, live) = verify_live_window(&win, false)?;
            Ok::<_, anyhow::Error>((hwnd as usize, live))
        })
        .await??;
        let out = run_powershell(
            UIA_READ_TREE_SCRIPT,
            &[
                ("QA_DESKTOP_HWND", hwnd_raw.to_string()),
                ("QA_DESKTOP_LIMIT", limit.clamp(1, 200).to_string()),
            ],
            Duration::from_secs(8),
            MAX_UIA_OUTPUT,
        )
        .await?;
        let raw_text = String::from_utf8_lossy(&out);
        let trimmed = raw_text.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }
        let parsed: Value = serde_json::from_str(trimmed).map_err(|err| {
            fault(
                "DESKTOP_BACKEND_FAILED",
                format!("invalid UIA tree JSON: {err}"),
            )
        })?;
        let items = match parsed {
            Value::Array(arr) => arr,
            Value::Object(_) => vec![parsed],
            _ => Vec::new(),
        };
        let pid_str = live.pid.map(|p| p.to_string()).unwrap_or_default();
        let mut nodes = Vec::with_capacity(items.len());
        for item in items.into_iter().take(limit) {
            let name = trim_utf8(item["name"].as_str().unwrap_or(""), 240);
            let role = trim_utf8(item["role"].as_str().unwrap_or(""), 120);
            let depth = item["depth"].as_u64().unwrap_or(0).min(u16::MAX as u64) as u16;
            let rid = item["runtime_id"].as_str().unwrap_or("").trim();
            let invokable = item["invokable"].as_bool().unwrap_or(false)
                && !rid.is_empty()
                && !pid_str.is_empty()
                && !guard::sensitive(&role, &name);
            nodes.push(SemanticNode {
                name,
                role,
                depth,
                action: invokable.then(|| SemanticRef {
                    bus: "uia".into(),
                    sender: pid_str.clone(),
                    path: rid.to_owned(),
                }),
            });
        }
        Ok(nodes)
    }

    async fn invoke(
        &self,
        window: &Window,
        node: &SemanticRef,
        cancel: &CancellationToken,
    ) -> Result<()> {
        guard::cancelled(cancel)?;
        if node.bus != "uia" || node.path.trim().is_empty() {
            return Err(fault("STALE_REF", "invalid UIA semantic reference"));
        }
        let win = window.clone();
        let (hwnd_raw, live) = tokio::task::spawn_blocking(move || {
            let (hwnd, live) = verify_live_window(&win, true)?;
            Ok::<_, anyhow::Error>((hwnd as usize, live))
        })
        .await??;
        if live.pid.map(|p| p.to_string()).as_deref() != Some(node.sender.as_str()) {
            return Err(fault(
                "STALE_REF",
                "UIA application no longer matches the window",
            ));
        }
        let res = run_powershell(
            UIA_INVOKE_SCRIPT,
            &[
                ("QA_DESKTOP_HWND", hwnd_raw.to_string()),
                ("QA_DESKTOP_RUNTIME_ID", node.path.clone()),
            ],
            Duration::from_secs(6),
            4096,
        )
        .await;
        match res {
            Ok(_) => Ok(()),
            Err(err) => {
                let msg = err.to_string();
                if msg.contains("STALE_REF") {
                    Err(fault("STALE_REF", "UIA control is stale or unavailable"))
                } else if msg.contains("SENSITIVE_WINDOW") {
                    Err(fault(
                        "SENSITIVE_WINDOW",
                        "UIA control is protected and cannot be invoked",
                    ))
                } else {
                    Err(err)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hwnd_roundtrip_and_virtual_coord_mapping() {
        let sample = 0x001a_2b3cusize as HWND;
        let id = format_hwnd(sample);
        assert_eq!(id, "hwnd-1a2b3c");
        assert_eq!(parse_hwnd(&id), Some(sample));
        assert_eq!(parse_hwnd("hwnd-0"), None);
        assert_eq!(parse_hwnd("x123"), None);

        assert_eq!(normalize_virtual_coord(0, 0, 1920), 0);
        assert_eq!(normalize_virtual_coord(1919, 0, 1920), 65_535);
        assert_eq!(normalize_virtual_coord(-1920, -1920, 3840), 0);
        assert_eq!(normalize_virtual_coord(1919, -1920, 3840), 65_535);
    }

    #[test]
    fn maps_allowed_keys_and_windows_apps() {
        assert_eq!(map_virtual_key("Return"), Some((VK_RETURN, false)));
        assert_eq!(map_virtual_key("Delete"), Some((VK_DELETE, true)));
        assert_eq!(map_virtual_key("F12"), Some((VK_F1 + 11, false)));
        assert_eq!(map_virtual_key("s"), Some((b'S' as VIRTUAL_KEY, false)));
        assert_eq!(map_virtual_key("unknown"), None);

        assert_eq!(map_windows_app("calc"), Some("calc.exe"));
        assert_eq!(map_windows_app("notepad"), Some("notepad.exe"));
        assert_eq!(map_windows_app("msedge"), Some("msedge.exe"));
        assert_eq!(map_windows_app("cmd"), None);
    }

    #[test]
    fn list_windows_and_powershell_resolution_succeed_on_windows() {
        let ps = resolve_powershell();
        assert!(ps.is_file(), "expected pwsh or powershell at {}", ps.display());
        let windows = list_windows_sync();
        // Even in headless/CI desktops this should cleanly return without panicking.
        assert!(windows.len() <= 200);
        for w in &windows {
            assert!(w.window_id.starts_with("hwnd-"));
        }
    }

    #[tokio::test]
    async fn full_desktop_screenshot_and_uia_tree_work_on_interactive_windows() {
        let backend = Windows;
        if let Ok(png) = backend.screenshot(None).await {
            let decoded = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
                .expect("valid PNG bytes from Windows screen capture");
            assert!(decoded.width() > 0 && decoded.height() > 0);
        }
        let windows = backend.windows().await.unwrap();
        if let Some(target) = windows
            .iter()
            .find(|w| w.visible && !guard::sensitive(&w.title, &w.app_class))
        {
            let nodes = backend.read_tree(target, 15).await.expect("UIA read_tree");
            assert!(!nodes.is_empty(), "expected UIA nodes for {}", target.title);
            assert!(nodes.len() <= 15);
        }
    }
}
