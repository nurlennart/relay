use windows_sys::Win32::{
    Foundation::*, Graphics::Gdi::MapWindowPoints, UI::WindowsAndMessaging::*,
};
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

/// Relay shortcuts that work while a remote desktop has the keyboard.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Command {
    PreviousTab,
    NextTab,
    /// Returns keyboard focus to Relay.
    Release,
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
pub fn install_shortcuts(main: HWND, ctx: eframe::egui::Context) {
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
