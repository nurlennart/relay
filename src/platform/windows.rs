//! Windows: native child windows for the embedded RDP control, and the keyboard hook that keeps
//! Relay's shortcuts working while a remote desktop has focus.
use super::{Command, Window};
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, Win32WindowHandle, WindowsDisplayHandle,
};
use std::path::PathBuf;
use windows_sys::Win32::{
    Foundation::*, Graphics::Gdi::MapWindowPoints, UI::WindowsAndMessaging::*,
};

pub const PROPORTIONAL_FONTS: &[&str] = &[r"C:\Windows\Fonts\segoeui.ttf"];
pub const MONOSPACE_FONTS: &[&str] = &[
    r"C:\Windows\Fonts\CascadiaMono.ttf",
    r"C:\Windows\Fonts\consola.ttf",
];
pub const EXTRA_PROGRAM_DIRS: &[&str] = &[];

/// Keeps a console program such as ssh.exe from opening a console window of its own.
pub fn background(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

/// The Windows OpenSSH Client feature, or another `ssh.exe` on `PATH`.
pub fn ssh_program() -> PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    let system = PathBuf::from(root).join(r"System32\OpenSSH\ssh.exe");
    if system.is_file() {
        return system;
    }
    super::find_program("ssh.exe").unwrap_or(system)
}

impl Window {
    pub fn from_hwnd(window: HWND) -> Option<Self> {
        let handle = Win32WindowHandle::new(std::num::NonZeroIsize::new(window as isize)?);
        Some(Self {
            window: RawWindowHandle::Win32(handle),
            display: RawDisplayHandle::Windows(WindowsDisplayHandle::new()),
        })
    }

    pub fn hwnd(self) -> HWND {
        match self.window {
            RawWindowHandle::Win32(handle) => handle.hwnd.get() as HWND,
            _ => std::ptr::null_mut(),
        }
    }
}

/// Gives keyboard focus back to Relay's window, away from a remote desktop.
pub fn focus(window: Window) {
    unsafe {
        windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus(window.hwnd());
    }
}

fn last_error(message: &str) -> String {
    format!("{message}: {}", std::io::Error::last_os_error())
}

pub(crate) fn bounds(window: HWND, parent: HWND) -> Result<[i32; 4], String> {
    unsafe {
        let mut rect: RECT = std::mem::zeroed();
        if GetWindowRect(window, &mut rect) == 0 {
            return Err(last_error("Cannot read native window bounds"));
        }
        SetLastError(0);
        if MapWindowPoints(
            std::ptr::null_mut(),
            parent,
            (&mut rect as *mut RECT).cast(),
            2,
        ) == 0
            && GetLastError() != 0
        {
            return Err(last_error("Cannot map native window coordinates"));
        }
        Ok([
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
        ])
    }
}

pub(crate) fn position(window: HWND, parent: HWND, rect: [i32; 4]) -> Result<(), String> {
    if bounds(window, parent)? != rect {
        let [x, y, w, h] = rect;
        unsafe {
            if SetWindowPos(
                window,
                std::ptr::null_mut(),
                x,
                y,
                w,
                h,
                SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED | SWP_NOSENDCHANGING,
            ) == 0
            {
                return Err(last_error("Cannot position the RDP viewport"));
            }
        }
    }
    Ok(())
}

/// Cuts `holes` out of a child window so Relay's own overlays drawn beneath it show through.
/// Rectangles are `[x, y, width, height]` in the window's client coordinates; none restores
/// the full window.
pub(crate) fn clip(window: HWND, size: [i32; 2], holes: &[[i32; 4]]) -> Result<(), String> {
    use windows_sys::Win32::Graphics::Gdi::{
        CombineRgn, CreateRectRgn, DeleteObject, RGN_DIFF, SetWindowRgn,
    };
    unsafe {
        if holes.is_empty() {
            if SetWindowRgn(window, std::ptr::null_mut(), 1) == 0 {
                return Err(last_error("Cannot restore the RDP viewport"));
            }
            return Ok(());
        }
        let region = CreateRectRgn(0, 0, size[0], size[1]);
        for &[x, y, w, h] in holes {
            let hole = CreateRectRgn(x, y, x + w, y + h);
            CombineRgn(region, region, hole, RGN_DIFF);
            DeleteObject(hole);
        }
        // On success Windows owns the region.
        if SetWindowRgn(window, region, 1) == 0 {
            DeleteObject(region);
            return Err(last_error("Cannot clip the RDP viewport"));
        }
    }
    Ok(())
}

/// Finds a top-level window of this process by its exact title, skipping `known` ones.
pub fn find_window(title: &str, known: &[Window]) -> Option<Window> {
    struct Search {
        title: Vec<u16>,
        known: Vec<HWND>,
        found: Option<HWND>,
    }
    unsafe extern "system" fn each(window: HWND, search: LPARAM) -> windows_sys::core::BOOL {
        let search = unsafe { &mut *(search as *mut Search) };
        let mut process = 0;
        let mut text = [0u16; 512];
        unsafe {
            GetWindowThreadProcessId(window, &mut process);
            let len = GetWindowTextW(window, text.as_mut_ptr(), text.len() as i32);
            if process == std::process::id()
                && text[..len.max(0) as usize] == search.title[..]
                && !search.known.contains(&window)
            {
                search.found = Some(window);
                return 0;
            }
        }
        1
    }
    let mut search = Search {
        title: title.encode_utf16().collect(),
        known: known.iter().map(|w| w.hwnd()).collect(),
        found: None,
    };
    unsafe {
        EnumWindows(Some(each), &mut search as *mut Search as LPARAM);
    }
    search.found.and_then(Window::from_hwnd)
}

static MAIN: std::sync::atomic::AtomicPtr<std::ffi::c_void> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
static PENDING: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
static CONTEXT: std::sync::OnceLock<eframe::egui::Context> = std::sync::OnceLock::new();

fn command(key: u16, ctrl: bool, alt: bool) -> Option<Command> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{VK_HOME, VK_NEXT, VK_PRIOR};
    if !(ctrl && alt) {
        return None;
    }
    match key {
        VK_PRIOR => Some(Command::PreviousTab),
        VK_NEXT => Some(Command::NextTab),
        VK_HOME => Some(Command::Release),
        _ => None,
    }
}

/// Watches for Ctrl+Alt+PageUp/PageDown/Home while a remote desktop inside Relay has focus.
/// A low-level hook sees these keys before the RDP control, whichever thread owns its input.
pub fn install_shortcuts(main: Window, ctx: eframe::egui::Context) {
    let main = main.hwnd();
    if main.is_null() || CONTEXT.set(ctx).is_err() {
        return;
    }
    MAIN.store(main, std::sync::atomic::Ordering::Relaxed);
    unsafe {
        SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard), std::ptr::null_mut(), 0);
    }
}

/// Takes the shortcut pressed since the last call.
pub fn take_command() -> Option<Command> {
    match PENDING.swap(0, std::sync::atomic::Ordering::Relaxed) {
        1 => Some(Command::PreviousTab),
        2 => Some(Command::NextTab),
        3 => Some(Command::Release),
        _ => None,
    }
}

/// Whether a child of Relay's window, such as a remote desktop, has keyboard focus.
fn remote_focused() -> bool {
    let main = MAIN.load(std::sync::atomic::Ordering::Relaxed);
    unsafe {
        if main.is_null() || GetForegroundWindow() != main {
            return false;
        }
        let mut info: GUITHREADINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<GUITHREADINFO>() as u32;
        let thread = GetWindowThreadProcessId(main, std::ptr::null_mut());
        GetGUIThreadInfo(thread, &mut info) != 0
            && !info.hwndFocus.is_null()
            && info.hwndFocus != main
            && IsChild(main, info.hwndFocus) != 0
    }
}

unsafe extern "system" fn keyboard(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_MENU};
    if code == HC_ACTION as i32 {
        let info = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
        let down = |key| unsafe { GetAsyncKeyState(key as i32) } < 0;
        if let Some(command) = command(info.vkCode as u16, down(VK_CONTROL), down(VK_MENU))
            && remote_focused()
        {
            if info.flags & LLKHF_UP == 0 {
                PENDING.store(command as u8 + 1, std::sync::atomic::Ordering::Relaxed);
                if let Some(ctx) = CONTEXT.get() {
                    ctx.request_repaint();
                }
            }
            // Swallow both key edges so the remote desktop never sees the shortcut.
            return 1;
        }
    }
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{VK_HOME, VK_NEXT, VK_PRIOR, VK_TAB};
    #[test]
    fn clipping_cuts_holes_and_restores() {
        use windows_sys::Win32::Graphics::Gdi::{
            CreateRectRgn, DeleteObject, GetWindowRgn, PtInRegion,
        };
        unsafe {
            let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
            let window = CreateWindowExW(
                0,
                class.as_ptr(),
                std::ptr::null(),
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                400,
                300,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            );
            assert!(!window.is_null());
            clip(window, [400, 300], &[[300, 250, 90, 40]]).unwrap();
            let region = CreateRectRgn(0, 0, 0, 0);
            assert_ne!(GetWindowRgn(window, region), 0);
            assert_ne!(
                PtInRegion(region, 10, 10),
                0,
                "outside the hole stays visible"
            );
            assert_eq!(PtInRegion(region, 320, 260), 0, "the hole is cut out");
            clip(window, [400, 300], &[]).unwrap();
            assert_eq!(GetWindowRgn(window, region), 0, "no region after restoring");
            DeleteObject(region);
            DestroyWindow(window);
        }
    }
    #[test]
    fn shortcuts_need_ctrl_and_alt() {
        assert_eq!(command(VK_PRIOR, true, true), Some(Command::PreviousTab));
        assert_eq!(command(VK_NEXT, true, true), Some(Command::NextTab));
        assert_eq!(command(VK_HOME, true, true), Some(Command::Release));
        assert_eq!(command(VK_NEXT, true, false), None);
        assert_eq!(command(VK_HOME, false, true), None);
        assert_eq!(command(VK_TAB, true, true), None);
        assert!(!remote_focused(), "no Relay window is registered in tests");
    }
}
