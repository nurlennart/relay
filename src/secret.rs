//! Keeps an opted-in MSSQL connection string, encrypted with DPAPI for the current Windows user.
use std::path::{Path, PathBuf};
use windows_sys::Win32::{
    Foundation::LocalFree,
    Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    },
};

// Scopes the encrypted data to Relay so other DPAPI blobs of the user cannot be swapped in.
const ENTROPY: &[u8] = b"Relay MSSQL connection string";

pub fn path() -> PathBuf {
    crate::model::data_path().with_file_name("mssql.bin")
}

pub fn save(path: &Path, connection: &str) -> Result<(), String> {
    crate::model::atomic_write(path, &transform(connection.as_bytes(), true)?)
}

pub fn load(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    String::from_utf8(transform(&bytes, false).ok()?).ok()
}

pub fn forget(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
        _ => Ok(()),
    }
}

fn transform(data: &[u8], protect: bool) -> Result<Vec<u8>, String> {
    let blob = |bytes: &[u8]| CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr().cast_mut(),
    };
    let (input, entropy) = (blob(data), blob(ENTROPY));
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    unsafe {
        let ok = if protect {
            CryptProtectData(
                &input,
                std::ptr::null(),
                &entropy,
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                &entropy,
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        LocalFree(output.pbData.cast());
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stores_encrypted_and_forgets() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("secret-test-{}.bin", std::process::id()));
        let connection = "Server=tcp:db.example.com,1433;User ID=relay;Password=hunter2";
        save(&path, connection).unwrap();
        let stored = std::fs::read(&path).unwrap();
        assert!(!stored.windows(7).any(|w| w == b"hunter2"));
        assert_eq!(load(&path).as_deref(), Some(connection));
        std::fs::write(&path, b"tampered").unwrap();
        assert_eq!(load(&path), None);
        forget(&path).unwrap();
        assert!(!path.exists());
        forget(&path).unwrap();
    }
}
