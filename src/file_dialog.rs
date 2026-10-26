use std::path::PathBuf;
use windows::{
    Win32::{
        Foundation::{ERROR_CANCELLED, HWND, RPC_E_CHANGED_MODE},
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
            CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize,
        },
        UI::Shell::{
            Common::COMDLG_FILTERSPEC, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_NOCHANGEDIR,
            FOS_OVERWRITEPROMPT, FOS_PATHMUSTEXIST, FileOpenDialog, FileSaveDialog, IFileDialog,
            IFileOpenDialog, IFileSaveDialog, SIGDN_FILESYSPATH,
        },
    },
    core::{HSTRING, Interface, PCWSTR},
};

/// Balances a successful CoInitializeEx. The UI thread is usually already an STA.
struct Apartment(bool);
impl Apartment {
    fn new() -> Result<Self, String> {
        let result =
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
        if result == RPC_E_CHANGED_MODE {
            Ok(Self(false))
        } else {
            result
                .ok()
                .map_err(|e| format!("File picker failed: {e}"))?;
            Ok(Self(true))
        }
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

fn error(e: windows::core::Error) -> String {
    format!("File picker failed: {e}")
}

pub fn choose(
    parent: windows_sys::Win32::Foundation::HWND,
    save: bool,
    database: bool,
) -> Result<Option<PathBuf>, String> {
    let _apartment = Apartment::new()?;
    let dialog = create(save, database)?;
    unsafe {
        match dialog.Show(Some(HWND(parent))) {
            Err(e) if e.code() == ERROR_CANCELLED.to_hresult() => return Ok(None),
            result => result.map_err(error)?,
        }
        let name = dialog
            .GetResult()
            .and_then(|item| item.GetDisplayName(SIGDN_FILESYSPATH))
            .map_err(error)?;
        let path = name.to_string();
        CoTaskMemFree(Some(name.0 as _));
        path.map(|p| Some(PathBuf::from(p)))
            .map_err(|e| format!("File picker returned an invalid path: {e}"))
    }
}

/// Creates a configured CSV or SQLite picker. Requires an initialized apartment.
fn create(save: bool, database: bool) -> Result<IFileDialog, String> {
    let (filters, extension) = if database {
        (
            [
                ("SQLite database", "*.sqlite;*.db;*.sqlite3"),
                ("All files", "*.*"),
            ],
            "sqlite",
        )
    } else {
        ([("CSV file", "*.csv"), ("All files", "*.*")], "csv")
    };
    let filters = filters.map(|(name, spec)| (HSTRING::from(name), HSTRING::from(spec)));
    let specs = filters.each_ref().map(|(name, spec)| COMDLG_FILTERSPEC {
        pszName: PCWSTR(name.as_ptr()),
        pszSpec: PCWSTR(spec.as_ptr()),
    });
    unsafe {
        let dialog: IFileDialog = if save {
            CoCreateInstance::<_, IFileSaveDialog>(&FileSaveDialog, None, CLSCTX_INPROC_SERVER)
                .and_then(|d| d.cast())
        } else {
            CoCreateInstance::<_, IFileOpenDialog>(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)
                .and_then(|d| d.cast())
        }
        .map_err(error)?;
        dialog.SetFileTypes(&specs).map_err(error)?;
        dialog
            .SetDefaultExtension(&HSTRING::from(extension))
            .map_err(error)?;
        if save {
            dialog
                .SetFileName(&HSTRING::from("connections.csv"))
                .map_err(error)?;
        }
        let mut options = dialog.GetOptions().map_err(error)?
            | FOS_FORCEFILESYSTEM
            | FOS_NOCHANGEDIR
            | FOS_PATHMUSTEXIST;
        if save {
            options |= FOS_OVERWRITEPROMPT;
        } else if database {
            // A missing SQLite file is created when the library opens.
            options &= !FOS_FILEMUSTEXIST;
        } else {
            options |= FOS_FILEMUSTEXIST;
        }
        dialog.SetOptions(options).map_err(error)?;
        Ok(dialog)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::Shell::FILEOPENDIALOGOPTIONS;
    use windows_sys::{
        Win32::{
            Foundation::{HWND as Window, LPARAM, TRUE},
            UI::WindowsAndMessaging::{
                EnumWindows, GetClassNameW, GetWindowThreadProcessId, IsWindowVisible,
                PostMessageW, WM_CLOSE,
            },
        },
        core::BOOL,
    };

    #[test]
    fn pickers_use_expected_options() {
        let _apartment = Apartment::new().unwrap();
        let options =
            |save, database| unsafe { create(save, database).unwrap().GetOptions().unwrap() };
        let has = |o: FILEOPENDIALOGOPTIONS, flag| o.contains(flag);
        let export = options(true, false);
        assert!(has(export, FOS_OVERWRITEPROMPT) && has(export, FOS_FORCEFILESYSTEM));
        assert!(has(options(false, false), FOS_FILEMUSTEXIST));
        let database = options(false, true);
        assert!(!has(database, FOS_FILEMUSTEXIST) && has(database, FOS_NOCHANGEDIR));
    }

    #[test]
    fn cancelling_the_picker_returns_none() {
        // Close the dialog from another thread once it appears in this process.
        let closer = std::thread::spawn(|| {
            unsafe extern "system" fn close(window: Window, closed: LPARAM) -> BOOL {
                let mut process = 0;
                let mut class = [0u16; 16];
                unsafe {
                    GetWindowThreadProcessId(window, &mut process);
                    let len = GetClassNameW(window, class.as_mut_ptr(), class.len() as i32);
                    if process == std::process::id()
                        && String::from_utf16_lossy(&class[..len as usize]) == "#32770"
                        && IsWindowVisible(window) != 0
                    {
                        PostMessageW(window, WM_CLOSE, 0, 0);
                        *(closed as *mut bool) = true;
                    }
                }
                TRUE
            }
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            let mut closed = false;
            while !closed && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(100));
                unsafe { EnumWindows(Some(close), &mut closed as *mut bool as LPARAM) };
            }
            closed
        });
        assert_eq!(choose(std::ptr::null_mut(), true, false), Ok(None));
        assert!(closer.join().unwrap());
    }
}
