//! Keeps opted-in secrets in the system credential store: Windows Credential Manager, the
//! macOS Keychain, or the Secret Service on Linux. Server library connection strings and
//! remote logins for saved connections both live here; the connection library itself never
//! holds them.
use serde::{Deserialize, Serialize};

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

/// A remote username and password kept for a library connection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConnectionLogin {
    pub username: String,
    pub password: String,
}

#[derive(Serialize, Deserialize)]
struct StoredLogin {
    v: u8,
    #[serde(default)]
    username: String,
    password: String,
}

fn connection_entry(id: u64) -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, &format!("connection:{id}"))
        .map_err(|e| format!("The system credential store is unavailable: {e}"))
}

fn encode_login(login: &ConnectionLogin) -> Result<String, String> {
    serde_json::to_string(&StoredLogin {
        v: 1,
        username: login.username.clone(),
        password: login.password.clone(),
    })
    .map_err(|e| e.to_string())
}

fn decode_login(raw: &str) -> ConnectionLogin {
    if let Ok(stored) = serde_json::from_str::<StoredLogin>(raw)
        && stored.v == 1
    {
        return ConnectionLogin {
            username: stored.username,
            password: stored.password,
        };
    }
    // Plain passwords from the first Remember build, before username was stored.
    ConnectionLogin {
        username: String::new(),
        password: raw.into(),
    }
}

/// Stores a remote login for library connection `id`.
pub fn save_connection_login(id: u64, login: &ConnectionLogin) -> Result<(), String> {
    connection_entry(id)?
        .set_password(&encode_login(login)?)
        .map_err(|e| e.to_string())
}

/// The login saved for library connection `id`, if any.
pub fn load_connection_login(id: u64) -> Option<ConnectionLogin> {
    Some(decode_login(&connection_entry(id).ok()?.get_password().ok()?))
}

/// Removes any login saved for library connection `id`.
pub fn forget_connection_password(id: u64) -> Result<(), String> {
    match connection_entry(id)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// Fills `username` / `password` from the credential store when `id` is a saved connection
/// that has one, and sets `remember` so the sign-in form keeps the box ticked. An empty
/// stored username leaves the field unchanged (legacy password-only entries).
pub fn fill_connection_login(
    id: u64,
    username: &mut String,
    password: &mut String,
    remember: &mut bool,
) {
    if id == 0 {
        return;
    }
    let Some(saved) = load_connection_login(id) else {
        return;
    };
    if !saved.username.is_empty() {
        *username = saved.username;
    }
    *password = saved.password;
    *remember = true;
}

/// Saves or forgets the login for library connection `id` according to `remember`.
/// Ad-hoc connections (`id == 0`) are left alone.
pub fn persist_connection_login(
    id: u64,
    login: &ConnectionLogin,
    remember: bool,
) -> Result<(), String> {
    if id == 0 {
        return Ok(());
    }
    if remember {
        if login.password.is_empty() {
            forget_connection_password(id)
        } else {
            save_connection_login(id, login)
        }
    } else {
        forget_connection_password(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forgetting_a_missing_connection_password_is_ok() {
        assert!(forget_connection_password(u64::MAX - 7).is_ok());
        assert!(load_connection_login(u64::MAX - 7).is_none());
    }

    #[test]
    fn persist_skips_ad_hoc_connections() {
        assert!(
            persist_connection_login(
                0,
                &ConnectionLogin {
                    username: "a".into(),
                    password: "secret".into(),
                },
                true
            )
            .is_ok()
        );
        assert!(load_connection_login(0).is_none());
    }

    #[test]
    fn connection_login_round_trips_in_the_credential_store() {
        let id = u64::MAX - 11;
        let _ = forget_connection_password(id);
        let login = ConnectionLogin {
            username: "alex".into(),
            password: "relay-test-password".into(),
        };
        save_connection_login(id, &login).expect("credential store should accept a test login");
        assert_eq!(load_connection_login(id).as_ref(), Some(&login));
        forget_connection_password(id).unwrap();
        assert!(load_connection_login(id).is_none());
    }

    #[test]
    fn legacy_plain_passwords_still_load() {
        let id = u64::MAX - 15;
        let _ = forget_connection_password(id);
        connection_entry(id)
            .unwrap()
            .set_password("plain-legacy")
            .unwrap();
        let mut username = "keep".into();
        let mut password = String::new();
        let mut remember = false;
        fill_connection_login(id, &mut username, &mut password, &mut remember);
        assert_eq!(username, "keep");
        assert_eq!(password, "plain-legacy");
        assert!(remember);
        let _ = forget_connection_password(id);
    }

    #[test]
    fn fill_marks_remember_and_restores_username() {
        let id = u64::MAX - 13;
        let _ = forget_connection_password(id);
        save_connection_login(
            id,
            &ConnectionLogin {
                username: "bob".into(),
                password: "saved".into(),
            },
        )
        .unwrap();
        let mut username = "alice".into();
        let mut password = String::new();
        let mut remember = false;
        fill_connection_login(id, &mut username, &mut password, &mut remember);
        assert_eq!(username, "bob");
        assert_eq!(password, "saved");
        assert!(remember);
        let _ = forget_connection_password(id);
    }

    #[test]
    fn decode_rejects_other_versions_as_legacy_password() {
        let raw = r#"{"v":2,"username":"x","password":"y"}"#;
        assert_eq!(
            decode_login(raw),
            ConnectionLogin {
                username: String::new(),
                password: raw.into(),
            }
        );
    }
}
