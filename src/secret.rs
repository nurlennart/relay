//! Keeps an opted-in MSSQL connection string in the system credential store: Windows Credential
//! Manager, the macOS Keychain, or the Secret Service on Linux.
const SERVICE: &str = "Relay";
const ACCOUNT: &str = "MSSQL connection string";

fn entry() -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, ACCOUNT)
        .map_err(|e| format!("The system credential store is unavailable: {e}"))
}

pub fn save(connection: &str) -> Result<(), String> {
    entry()?.set_password(connection).map_err(|e| e.to_string())
}

pub fn load() -> Option<String> {
    entry().ok()?.get_password().ok()
}

pub fn forget() -> Result<(), String> {
    match entry()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}
