use std::path::PathBuf;
use windows::{
    Win32::{
        Foundation::{ERROR_CANCELLED, HWND, RPC_E_CHANGED_MODE},
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
            CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize,
        },
        UI::Shell::{
            Common::COMDLG_FILTERSPEC, FOS_ALLOWMULTISELECT, FOS_FILEMUSTEXIST,
            FOS_FORCEFILESYSTEM, FOS_NOCHANGEDIR, FOS_OVERWRITEPROMPT, FOS_PATHMUSTEXIST,
            FileOpenDialog, FileSaveDialog, IFileDialog, IFileOpenDialog, IFileSaveDialog,
            IShellItem, SIGDN_FILESYSPATH,
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

/// What a picker is for, which sets its file types and options.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    ImportCsv,
    ExportCsv,
    /// An existing or new SQLite library.
    Database,
    /// One or more Remote Desktop files.
    RdpFiles,
    /// An SSH private key.
    Key,
}

impl Kind {
    fn save(self) -> bool {
        self == Self::ExportCsv
    }
    fn filters(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::ImportCsv | Self::ExportCsv => &[("CSV file", "*.csv"), ("All files", "*.*")],
            Self::Database => &[
                ("SQLite database", "*.sqlite;*.db;*.sqlite3"),
                ("All files", "*.*"),
            ],
            Self::RdpFiles => &[("Remote Desktop file", "*.rdp"), ("All files", "*.*")],
            Self::Key => &[("All files", "*.*")],
        }
    }
}

/// Shows a picker and returns the chosen paths; an empty list means it was cancelled.
pub fn choose(
    parent: windows_sys::Win32::Foundation::HWND,
    kind: Kind,
) -> Result<Vec<PathBuf>, String> {
    let _apartment = Apartment::new()?;
    let dialog = create(kind)?;
    unsafe {
        match dialog.Show(Some(HWND(parent))) {
            Err(e) if e.code() == ERROR_CANCELLED.to_hresult() => return Ok(vec![]),
            result => result.map_err(error)?,
        }
        let items: Vec<IShellItem> = if kind == Kind::RdpFiles {
            let results = dialog
                .cast::<IFileOpenDialog>()
                .and_then(|d| d.GetResults())
                .map_err(error)?;
            let count = results.GetCount().map_err(error)?;
            (0..count)
                .map(|i| results.GetItemAt(i))
                .collect::<Result<_, _>>()
                .map_err(error)?
        } else {
            vec![dialog.GetResult().map_err(error)?]
        };
        items
            .iter()
            .map(|item| {
                let name = item.GetDisplayName(SIGDN_FILESYSPATH).map_err(error)?;
                let path = name.to_string();
                CoTaskMemFree(Some(name.0 as _));
                path.map(PathBuf::from)
                    .map_err(|e| format!("File picker returned an invalid path: {e}"))
            })
            .collect()
    }
}

/// Creates a configured picker. Requires an initialized apartment.
fn create(kind: Kind) -> Result<IFileDialog, String> {
    let filters: Vec<_> = kind
        .filters()
        .iter()
        .map(|(name, spec)| (HSTRING::from(*name), HSTRING::from(*spec)))
        .collect();
    let specs: Vec<_> = filters
        .iter()
        .map(|(name, spec)| COMDLG_FILTERSPEC {
            pszName: PCWSTR(name.as_ptr()),
            pszSpec: PCWSTR(spec.as_ptr()),
        })
        .collect();
    unsafe {
        let dialog: IFileDialog = if kind.save() {
            CoCreateInstance::<_, IFileSaveDialog>(&FileSaveDialog, None, CLSCTX_INPROC_SERVER)
                .and_then(|d| d.cast())
        } else {
            CoCreateInstance::<_, IFileOpenDialog>(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)
                .and_then(|d| d.cast())
        }
        .map_err(error)?;
        dialog.SetFileTypes(&specs).map_err(error)?;
        let extension = match kind {
            Kind::ImportCsv | Kind::ExportCsv => Some("csv"),
            Kind::Database => Some("sqlite"),
            Kind::RdpFiles => Some("rdp"),
            Kind::Key => None,
        };
        if let Some(extension) = extension {
            dialog
                .SetDefaultExtension(&HSTRING::from(extension))
                .map_err(error)?;
        }
        if kind.save() {
            dialog
                .SetFileName(&HSTRING::from("connections.csv"))
                .map_err(error)?;
        }
        let mut options = dialog.GetOptions().map_err(error)?
            | FOS_FORCEFILESYSTEM
            | FOS_NOCHANGEDIR
            | FOS_PATHMUSTEXIST;
        match kind {
            Kind::ExportCsv => options |= FOS_OVERWRITEPROMPT,
            // A missing SQLite file is created when the library opens.
            Kind::Database => options &= !FOS_FILEMUSTEXIST,
            Kind::RdpFiles => options |= FOS_FILEMUSTEXIST | FOS_ALLOWMULTISELECT,
            Kind::ImportCsv | Kind::Key => options |= FOS_FILEMUSTEXIST,
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
        let options = |kind| unsafe { create(kind).unwrap().GetOptions().unwrap() };
        let has = |o: FILEOPENDIALOGOPTIONS, flag| o.contains(flag);
        let export = options(Kind::ExportCsv);
        assert!(has(export, FOS_OVERWRITEPROMPT) && has(export, FOS_FORCEFILESYSTEM));
        assert!(has(options(Kind::ImportCsv), FOS_FILEMUSTEXIST));
        assert!(!has(options(Kind::ImportCsv), FOS_ALLOWMULTISELECT));
        let database = options(Kind::Database);
        assert!(!has(database, FOS_FILEMUSTEXIST) && has(database, FOS_NOCHANGEDIR));
        assert!(has(options(Kind::RdpFiles), FOS_ALLOWMULTISELECT));
        assert!(has(options(Kind::Key), FOS_FILEMUSTEXIST));
    }

    #[test]
    fn cancelling_the_picker_returns_nothing() {
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
        assert_eq!(choose(std::ptr::null_mut(), Kind::ExportCsv), Ok(vec![]));
        assert!(closer.join().unwrap());
    }
}
