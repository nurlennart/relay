//! Keeps opted-in server connection strings in the system credential store: Windows Credential
//! Manager, the macOS Keychain, or the Secret Service on Linux.
const SERVICE: &str = "Relay";

/// The credential store entry for each kind of server library.
#[derive(Clone, Copy)]
pub enum Account {
    Mssql,
    Mysql,
}

impl Account {
    fn name(self) -> &'static str {
        match self {
            Self::Mssql => "MSSQL connection string",
            Self::Mysql => "MySQL connection URL",
        }
    }
}

fn entry(account: Account) -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, account.name())
        .map_err(|e| format!("The system credential store is unavailable: {e}"))
}

pub fn save(account: Account, connection: &str) -> Result<(), String> {
    entry(account)?
        .set_password(connection)
        .map_err(|e| e.to_string())
}

pub fn load(account: Account) -> Option<String> {
    entry(account).ok()?.get_password().ok()
}

pub fn forget(account: Account) -> Result<(), String> {
    match entry(account)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}
