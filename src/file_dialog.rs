use std::path::PathBuf;
use windows_sys::Win32::{Foundation::HWND, UI::Controls::Dialogs::*};

pub fn choose(parent: HWND, save: bool, database: bool) -> Result<Option<PathBuf>, String> {
    let mut buffer = vec![0u16; 32768];
    if save {
        for (i, c) in "connections.csv".encode_utf16().enumerate() {
            buffer[i] = c;
        }
    }
    let filter: Vec<u16> = if database {
        "SQLite database\0*.sqlite;*.db;*.sqlite3\0All files\0*.*\0\0"
    } else {
        "CSV file\0*.csv\0All files\0*.*\0\0"
    }
    .encode_utf16()
    .collect();
    let extension: Vec<u16> = if database { "sqlite\0" } else { "csv\0" }
        .encode_utf16()
        .collect();
    unsafe {
        let mut ofn: OPENFILENAMEW = std::mem::zeroed();
        ofn.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
        ofn.hwndOwner = parent;
        ofn.lpstrFile = buffer.as_mut_ptr();
        ofn.nMaxFile = buffer.len() as u32;
        ofn.lpstrFilter = filter.as_ptr();
        ofn.lpstrDefExt = extension.as_ptr();
        ofn.Flags = OFN_EXPLORER
            | OFN_NOCHANGEDIR
            | OFN_PATHMUSTEXIST
            | if save {
                OFN_OVERWRITEPROMPT
            } else if database {
                0
            } else {
                OFN_FILEMUSTEXIST
            };
        let success = if save {
            GetSaveFileNameW(&mut ofn)
        } else {
            GetOpenFileNameW(&mut ofn)
        };
        if success == 0 {
            let code = CommDlgExtendedError();
            return if code == 0 {
                Ok(None)
            } else {
                Err(format!("File picker failed (Windows error {code})."))
            };
        }
    }
    let len = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
    Ok(Some(PathBuf::from(String::from_utf16_lossy(
        &buffer[..len],
    ))))
}
