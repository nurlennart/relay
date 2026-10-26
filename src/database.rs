use crate::model::{self, Connection};
use std::{
    path::PathBuf,
    sync::mpsc::{self, Receiver},
    time::Duration,
};
use tokio_util::compat::TokioAsyncWriteCompatExt;

#[derive(Clone)]
pub enum Location {
    Local(PathBuf),
    Sqlite(PathBuf),
    Mssql(String),
}
impl Location {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Local(_) => "Personal library",
            Self::Sqlite(_) => "SQLite library",
            Self::Mssql(_) => "MSSQL team library",
        }
    }
}
pub struct Snapshot {
    pub connections: Vec<Connection>,
    pub revision: i64,
}
const CONFLICT: &str = "Someone changed this library since your last refresh. \
    Your changes were not written. Export your unsaved changes, refresh, and apply them again.";

pub fn start(
    location: Location,
    revision: i64,
    next: Option<Vec<Connection>>,
    ctx: eframe::egui::Context,
) -> Receiver<Result<Snapshot, String>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = match &location {
            Location::Local(path) => match next {
                Some(connections) => model::save(path, &connections).map(|_| Snapshot {
                    connections,
                    revision: revision + 1,
                }),
                None => model::load(path).map(|connections| Snapshot {
                    connections,
                    revision: 0,
                }),
            },
            Location::Sqlite(path) => sqlite(path, revision, next),
            Location::Mssql(connection) => mssql(connection, revision, next),
        };
        let _ = tx.send(result);
        ctx.request_repaint();
    });
    rx
}

fn sqlite(
    path: &std::path::Path,
    revision: i64,
    next: Option<Vec<Connection>>,
) -> Result<Snapshot, String> {
    let connection = rusqlite::Connection::open(path).map_err(|e| e.to_string())?;
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(|e| e.to_string())?;
    // Rollback journals support file locking on SMB; WAL requires shared memory on one machine.
    connection
        .execute_batch(
            "PRAGMA journal_mode=DELETE;
             PRAGMA synchronous=FULL;
             CREATE TABLE IF NOT EXISTS relay_library (
                 id INTEGER PRIMARY KEY CHECK(id=1),
                 revision INTEGER NOT NULL,
                 payload TEXT NOT NULL
             );
             INSERT OR IGNORE INTO relay_library(id, revision, payload) VALUES(1, 0, '[]');",
        )
        .map_err(|e| e.to_string())?;
    if let Some(connections) = next {
        let json = serde_json::to_string(&connections).map_err(|e| e.to_string())?;
        let changed = connection
            .execute(
                "UPDATE relay_library SET revision=revision+1, payload=?1 \
                 WHERE id=1 AND revision=?2",
                rusqlite::params![json, revision],
            )
            .map_err(|e| e.to_string())?;
        if changed != 1 {
            return Err(CONFLICT.into());
        }
        Ok(Snapshot {
            connections,
            revision: revision + 1,
        })
    } else {
        let (revision, payload): (i64, String) = connection
            .query_row(
                "SELECT revision, payload FROM relay_library WHERE id=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|e| e.to_string())?;
        Ok(Snapshot {
            connections: model::decode(payload.as_bytes())?,
            revision,
        })
    }
}

fn mssql(
    connection: &str,
    revision: i64,
    next: Option<Vec<Connection>>,
) -> Result<Snapshot, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let work = async {
        let mut config = tiberius::Config::from_ado_string(connection).map_err(|_| {
            "Invalid MSSQL connection string. Use the documented ADO.NET format.".to_string()
        })?;
        config.encryption(tiberius::EncryptionLevel::Required);
        let tcp = tokio::net::TcpStream::connect(config.get_addr())
            .await
            .map_err(|e| format!("MSSQL network connection failed: {e}"))?;
        tcp.set_nodelay(true).map_err(|e| e.to_string())?;
        let mut client = tiberius::Client::connect(config, tcp.compat_write())
            .await
            .map_err(|_| {
                "MSSQL authentication/TLS failed. Check the server name, credentials, and \
                 trusted certificate. The connection string is not logged."
                    .to_string()
            })?;
        if let Some(connections) = next {
            let json = serde_json::to_string(&connections).map_err(|e| e.to_string())?;
            let result = client
                .execute(
                    "UPDATE dbo.RelayLibrary SET Revision=Revision+1, Payload=@P1 \
                     WHERE Id=1 AND Revision=@P2",
                    &[&json, &revision],
                )
                .await
                .map_err(|e| format!("MSSQL save failed: {e}"))?;
            if result.total() != 1 {
                return Err(CONFLICT.into());
            }
            Ok(Snapshot {
                connections,
                revision: revision + 1,
            })
        } else {
            let row = client
                .simple_query("SELECT Revision, Payload FROM dbo.RelayLibrary WHERE Id=1")
                .await
                .map_err(|e| {
                    format!(
                        "Cannot read dbo.RelayLibrary. Run sql/mssql.sql once in your database. {e}"
                    )
                })?
                .into_row()
                .await
                .map_err(|e| e.to_string())?
                .ok_or("The MSSQL library row is missing. Run sql/mssql.sql.")?;
            let revision: i64 = row
                .try_get(0)
                .map_err(|e| e.to_string())?
                .ok_or("Missing library revision")?;
            let payload: &str = row
                .try_get(1)
                .map_err(|e| e.to_string())?
                .ok_or("Missing library data")?;
            Ok(Snapshot {
                connections: model::decode(payload.as_bytes())?,
                revision,
            })
        }
    };
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(15), work)
            .await
            .map_err(|_| {
                "MSSQL operation timed out. Refresh before retrying a save; \
                 the server may have committed it."
                    .to_string()
            })?
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sqlite_rejects_stale_writes_and_roundtrips() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("team-test-{}.sqlite", std::process::id()));
        let original = sqlite(&path, 0, None).unwrap();
        assert_eq!(original.revision, 0);
        let mut c = Connection::new();
        c.id = 1;
        c.name = "Test".into();
        c.host = "localhost".into();
        let first = sqlite(&path, 0, Some(vec![c])).unwrap();
        assert_eq!(first.revision, 1);
        assert!(sqlite(&path, 0, Some(vec![])).is_err());
        let read = sqlite(&path, 0, None).unwrap();
        assert_eq!(read.connections.len(), 1);
        assert_eq!(read.revision, 1);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn mssql_config_supports_sql_and_integrated_auth() {
        for connection in [
            "Server=tcp:db.example.com,1433;Database=Relay;Integrated Security=true;\
             Encrypt=true;TrustServerCertificate=false",
            "Server=tcp:db.example.com,1433;Database=Relay;User ID=relay;Password=test;\
             Encrypt=true",
        ] {
            assert!(tiberius::Config::from_ado_string(connection).is_ok());
        }
    }
}
