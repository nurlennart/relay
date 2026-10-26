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
    /// An ADO.NET connection string.
    Mssql(Server),
    /// A `mysql://` URL, which works with MySQL and MariaDB.
    Mysql(Server),
}

/// A database server library: how to reach it, and whether the connection is encrypted.
#[derive(Clone)]
pub struct Server {
    pub connection: String,
    /// Off only for servers on trusted networks that cannot offer TLS.
    pub tls: bool,
}
impl Location {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Local(_) => "Personal library",
            Self::Sqlite(_) => "SQLite library",
            Self::Mssql(_) => "MSSQL team library",
            Self::Mysql(_) => "MySQL team library",
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
            Location::Mssql(server) => mssql(server, revision, next),
            Location::Mysql(server) => mysql(server, revision, next),
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
    server: &Server,
    revision: i64,
    next: Option<Vec<Connection>>,
) -> Result<Snapshot, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let work = async {
        let config = mssql_config(server)?;
        let tcp = tokio::net::TcpStream::connect(config.get_addr())
            .await
            .map_err(|e| format!("MSSQL network connection failed: {e}"))?;
        tcp.set_nodelay(true).map_err(|e| e.to_string())?;
        let mut client = tiberius::Client::connect(config, tcp.compat_write())
            .await
            .map_err(|_| {
                if server.tls {
                    "MSSQL authentication/TLS failed. Check the server name, credentials, and \
                     trusted certificate. The connection string is not logged."
                } else {
                    "MSSQL authentication failed, or the server requires TLS. Check the server \
                     name and credentials. The connection string is not logged."
                }
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

/// The MSSQL configuration: TLS is required, or not used at all when turned off.
fn mssql_config(server: &Server) -> Result<tiberius::Config, String> {
    let mut config = tiberius::Config::from_ado_string(&server.connection).map_err(|_| {
        "Invalid MSSQL connection string. Use the documented ADO.NET format.".to_string()
    })?;
    config.encryption(if server.tls {
        tiberius::EncryptionLevel::Required
    } else {
        tiberius::EncryptionLevel::NotSupported
    });
    Ok(config)
}

/// Options for a MySQL URL. With TLS on, the connection is always encrypted, and without TLS
/// options in the URL the server certificate is verified against trusted roots. With TLS off,
/// TLS options in the URL are ignored.
fn mysql_opts(server: &Server) -> Result<mysql::Opts, String> {
    // Parse errors can quote parts of the URL, so they are not shown.
    let opts = mysql::Opts::from_url(&server.connection).map_err(|_| {
        "Invalid MySQL connection URL. Use mysql://user:password@host:3306/database.".to_string()
    })?;
    if opts.get_db_name().is_none_or(str::is_empty) {
        return Err("Add the database to the MySQL URL, as in mysql://user@host/relay.".into());
    }
    let ssl = server
        .tls
        .then(|| opts.get_ssl_opts().cloned().unwrap_or_default());
    let timeout = Some(Duration::from_secs(15));
    Ok(mysql::OptsBuilder::from_opts(opts)
        .ssl_opts(ssl)
        .tcp_connect_timeout(Some(Duration::from_secs(10)))
        .read_timeout(timeout)
        .write_timeout(timeout)
        .into())
}

fn mysql(
    server: &Server,
    revision: i64,
    next: Option<Vec<Connection>>,
) -> Result<Snapshot, String> {
    use mysql::prelude::Queryable;
    let mut client = mysql::Conn::new(mysql_opts(server)?)
        .map_err(|e| format!("MySQL connection failed: {e}. The URL is not logged."))?;
    if let Some(connections) = next {
        let json = serde_json::to_string(&connections).map_err(|e| e.to_string())?;
        // The revision always changes, so a matched row is also a changed row.
        client
            .exec_drop(
                "UPDATE RelayLibrary SET Revision=Revision+1, Payload=? \
                 WHERE Id=1 AND Revision=?",
                (json, revision),
            )
            .map_err(|e| {
                format!(
                    "MySQL save failed: {e}. Refresh before retrying; the server may have \
                     committed it."
                )
            })?;
        if client.affected_rows() != 1 {
            return Err(CONFLICT.into());
        }
        Ok(Snapshot {
            connections,
            revision: revision + 1,
        })
    } else {
        let (revision, payload): (i64, String) = client
            .query_first("SELECT Revision, Payload FROM RelayLibrary WHERE Id=1")
            .map_err(|e| {
                format!("Cannot read RelayLibrary. Run sql/mysql.sql once in your database. {e}")
            })?
            .ok_or("The MySQL library row is missing. Run sql/mysql.sql.")?;
        Ok(Snapshot {
            connections: model::decode(payload.as_bytes())?,
            revision,
        })
    }
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
    fn server(connection: &str, tls: bool) -> Server {
        Server {
            connection: connection.into(),
            tls,
        }
    }
    #[test]
    fn mssql_tls_is_required_unless_turned_off() {
        let connection = "Server=tcp:db.example.com,1433;Database=Relay;User ID=relay;\
                          Password=test;Encrypt=false";
        for tls in [true, false] {
            assert!(mssql_config(&server(connection, tls)).is_ok());
        }
        assert!(mssql_config(&server("not a connection string", true)).is_err());
    }
    #[test]
    fn mysql_urls_use_tls_unless_turned_off_and_name_a_database() {
        let url = "mysql://relay:secret@db.example.com:3306/relay";
        let opts = mysql_opts(&server(url, true)).unwrap();
        let ssl = opts.get_ssl_opts().expect("TLS is required");
        assert!(!ssl.accept_invalid_certs() && !ssl.skip_domain_validation());
        assert_eq!(opts.get_db_name(), Some("relay"));
        // TLS options in the URL are kept, or ignored when TLS is off.
        let url = "mysql://relay@db.example.com/relay?root_cert_path=/etc/relay/ca.pem\
                   &danger_skip_domain_validation=true";
        let opts = mysql_opts(&server(url, true)).unwrap();
        let ssl = opts.get_ssl_opts().unwrap();
        assert_eq!(
            ssl.root_cert_path(),
            Some(std::path::Path::new("/etc/relay/ca.pem"))
        );
        assert!(ssl.skip_domain_validation());
        assert!(
            mysql_opts(&server(url, false))
                .unwrap()
                .get_ssl_opts()
                .is_none()
        );
        for url in [
            "mysql://relay@db.example.com",
            "mysql://relay@db.example.com/",
            "Server=tcp:db.example.com,1433;Database=Relay",
        ] {
            assert!(mysql_opts(&server(url, true)).is_err(), "{url}");
        }
        let error = mysql_opts(&server(
            "mysql://relay:secret@db.example.com/relay?nope=secret",
            true,
        ));
        assert!(!error.unwrap_err().contains("secret"));
    }
    /// Needs a server: `RELAY_TEST_MSSQL='Server=...' cargo test -- --ignored mssql_live`.
    /// Set `RELAY_TEST_MSSQL_TLS=0` to connect without TLS. Runs sql/mssql.sql first, then
    /// checks reads, saves, and stale-write rejection.
    #[test]
    #[ignore]
    fn mssql_live_rejects_stale_writes_and_roundtrips() {
        let tls = std::env::var("RELAY_TEST_MSSQL_TLS").map_or(true, |v| v != "0");
        let server = server(
            &std::env::var("RELAY_TEST_MSSQL").expect("set RELAY_TEST_MSSQL"),
            tls,
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let config = mssql_config(&server).unwrap();
            let tcp = tokio::net::TcpStream::connect(config.get_addr())
                .await
                .unwrap();
            let mut client = tiberius::Client::connect(config, tcp.compat_write())
                .await
                .unwrap();
            let row = client
                .simple_query(
                    "SELECT encrypt_option FROM sys.dm_exec_connections WHERE session_id=@@SPID",
                )
                .await
                .unwrap()
                .into_row()
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                row.get::<&str, _>(0),
                Some(if tls { "TRUE" } else { "FALSE" })
            );
            client
                .simple_query("DROP TABLE IF EXISTS dbo.RelayLibrary")
                .await
                .unwrap();
            client
                .simple_query(include_str!("../sql/mssql.sql"))
                .await
                .unwrap()
                .into_results()
                .await
                .unwrap();
        });
        let original = mssql(&server, 0, None).unwrap();
        assert_eq!((original.revision, original.connections.len()), (0, 0));
        let mut c = Connection::new();
        c.id = 1;
        c.name = "Tëst".into();
        c.host = "localhost".into();
        assert_eq!(mssql(&server, 0, Some(vec![c])).unwrap().revision, 1);
        assert_eq!(
            mssql(&server, 0, Some(vec![])).err().as_deref(),
            Some(CONFLICT)
        );
        let read = mssql(&server, 0, None).unwrap();
        assert_eq!(
            (read.revision, read.connections[0].name.as_str()),
            (1, "Tëst")
        );
    }
    /// Needs a server: `RELAY_TEST_MYSQL=mysql://... cargo test -- --ignored mysql_live`.
    /// Set `RELAY_TEST_MYSQL_TLS=0` to connect without TLS. Runs sql/mysql.sql first, then
    /// checks reads, saves, and stale-write rejection.
    #[test]
    #[ignore]
    fn mysql_live_rejects_stale_writes_and_roundtrips() {
        use mysql::prelude::Queryable;
        let tls = std::env::var("RELAY_TEST_MYSQL_TLS").map_or(true, |v| v != "0");
        let server = server(
            &std::env::var("RELAY_TEST_MYSQL").expect("set RELAY_TEST_MYSQL"),
            tls,
        );
        let mut client = mysql::Conn::new(mysql_opts(&server).unwrap()).unwrap();
        let cipher: Option<(String, String)> = client
            .query_first("SHOW SESSION STATUS LIKE 'Ssl_cipher'")
            .unwrap();
        assert_eq!(
            cipher.is_some_and(|(_, c)| !c.is_empty()),
            tls,
            "TLS in use"
        );
        client
            .query_drop("DROP TABLE IF EXISTS RelayLibrary")
            .unwrap();
        for statement in include_str!("../sql/mysql.sql")
            .split(';')
            .map(|s| {
                s.lines()
                    .filter(|line| !line.trim_start().starts_with("--"))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .filter(|s| !s.trim().is_empty())
        {
            client.query_drop(statement).unwrap();
        }
        let original = mysql(&server, 0, None).unwrap();
        assert_eq!((original.revision, original.connections.len()), (0, 0));
        let mut c = Connection::new();
        c.id = 1;
        c.name = "Tëst".into();
        c.host = "localhost".into();
        let first = mysql(&server, 0, Some(vec![c])).unwrap();
        assert_eq!(first.revision, 1);
        assert_eq!(
            mysql(&server, 0, Some(vec![])).err().as_deref(),
            Some(CONFLICT)
        );
        let read = mysql(&server, 0, None).unwrap();
        assert_eq!(read.revision, 1);
        assert_eq!(read.connections[0].name, "Tëst");
        // The payload must stay valid JSON.
        assert!(
            client
                .query_drop("UPDATE RelayLibrary SET Payload='not json' WHERE Id=1")
                .is_err()
        );
    }
}
