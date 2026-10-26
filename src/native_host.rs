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
