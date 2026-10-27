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
    /// Where the library is, for people: its file, or its server and database. Never shows a
    /// password or the rest of a connection string.
    pub fn detail(&self) -> String {
        match self {
            Self::Local(path) | Self::Sqlite(path) => path.display().to_string(),
            Self::Mssql(server) => {
                let value = |keys: &[&str]| {
                    server.connection.split(';').find_map(|part| {
                        let (key, value) = part.split_once('=')?;
                        keys.contains(&key.trim().to_ascii_lowercase().as_str())
                            .then(|| value.trim().trim_matches(['"', '\'']).to_string())
                    })
                };
                let host = value(&[
                    "server",
                    "data source",
                    "address",
                    "addr",
                    "network address",
                ])
                .map(|h| h.strip_prefix("tcp:").unwrap_or(&h).to_string());
                let database = value(&["database", "initial catalog"]);
                [host, database]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" · ")
            }
            Self::Mysql(server) => mysql::Opts::from_url(&server.connection)
                .map(|opts| {
                    let host = format!("{}:{}", opts.get_ip_or_hostname(), opts.get_tcp_port());
                    match opts.get_db_name() {
                        Some(db) => format!("{host} · {db}"),
                        None => host,
                    }
                })
                .unwrap_or_default(),
        }
    }
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
        let tcp = mssql_connect(&config).await?;
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

/// SQL Server Browser's port, which tiberius gives for a named instance without a port.
const SQL_BROWSER_PORT: u16 = 1434;

/// Whether `config` names an instance without a port, so the SQL Server Browser has to say
/// which port it listens on. With a port, Relay connects to it directly.
fn needs_browser(config: &tiberius::Config) -> bool {
    config
        .get_addr()
        .rsplit_once(':')
        .is_some_and(|(_, port)| port == SQL_BROWSER_PORT.to_string())
}

/// Opens the network connection to the server: directly to its port, or, for a named instance
/// without one, to the port the SQL Server Browser gives for it.
async fn mssql_connect(config: &tiberius::Config) -> Result<tokio::net::TcpStream, String> {
    use tiberius::SqlBrowser;
    if needs_browser(config) {
        return tokio::net::TcpStream::connect_named(config)
            .await
            .map_err(|_| {
                "The SQL Server Browser did not answer for this named instance. Check that the \
                 SQL Server Browser service is running and UDP port 1434 is reachable, or give \
                 the instance's port instead, as in Server=tcp:host,port."
                    .to_string()
            });
    }
    let tcp = tokio::net::TcpStream::connect(config.get_addr())
        .await
        .map_err(|e| format!("MSSQL network connection failed: {e}"))?;
    tcp.set_nodelay(true).map_err(|e| e.to_string())?;
    Ok(tcp)
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
    #[test]
    fn library_details_name_the_place_but_never_the_password() {
        let mssql = Location::Mssql(server(
            "Server=tcp:sql01\\SQLEXPRESS;Database=Relay;User ID=relay;Password=hunter2",
            true,
        ));
        assert_eq!(mssql.detail(), "sql01\\SQLEXPRESS · Relay");
        let mssql = Location::Mssql(server(
            "data source = sql01,1433 ; Initial Catalog='Team';Integrated Security=true",
            true,
        ));
        assert_eq!(mssql.detail(), "sql01,1433 · Team");
        let mysql = Location::Mysql(server("mysql://relay:hunter2@db.example.com/relay", true));
        assert_eq!(mysql.detail(), "db.example.com:3306 · relay");
        for location in [&mssql, &mysql] {
            assert!(!location.detail().contains("hunter2"));
        }
        let sqlite = Location::Sqlite(PathBuf::from("/srv/team/relay.sqlite"));
        assert_eq!(sqlite.detail(), "/srv/team/relay.sqlite");
    }
    #[test]
    fn named_instances_ask_the_browser_unless_a_port_is_given() {
        let config = |server: &str| {
            mssql_config(&server_of(&format!(
                "Server={server};Database=Relay;User ID=relay;Password=test"
            )))
            .unwrap()
        };
        for named in ["tcp:sql01\\SQLEXPRESS", "sql01\\SQLEXPRESS"] {
            let c = config(named);
            assert!(needs_browser(&c), "{named}");
            assert_eq!(c.get_addr(), "sql01:1434");
        }
        for direct in [
            "tcp:sql01\\SQLEXPRESS,49733",
            "tcp:sql01,49733",
            "tcp:sql01",
        ] {
            assert!(!needs_browser(&config(direct)), "{direct}");
        }
        assert_eq!(
            config("tcp:sql01\\SQLEXPRESS,49733").get_addr(),
            "sql01:49733"
        );
        assert_eq!(config("tcp:sql01").get_addr(), "sql01:1433");
    }
    /// A stand-in SQL Server Browser on this computer answers for the instance with the port
    /// a listener waits on, and the connection goes there. Skipped when UDP 1434 is taken,
    /// as on a machine running the real Browser.
    #[test]
    fn named_instances_connect_to_the_port_the_browser_gives() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let Ok(browser) = tokio::net::UdpSocket::bind(("127.0.0.1", SQL_BROWSER_PORT)).await
            else {
                eprintln!("skipping: UDP 1434 is in use");
                return;
            };
            let instance = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = instance.local_addr().unwrap().port();
            tokio::spawn(async move {
                let mut request = [0u8; 256];
                let (len, from) = browser.recv_from(&mut request).await.unwrap();
                // A unicast request for one instance: 0x04, then its name.
                assert_eq!(&request[..len], b"\x04SQLEXPRESS");
                let data =
                    format!("ServerName;SQL01;InstanceName;SQLEXPRESS;IsClustered;No;tcp;{port};;");
                let mut reply = vec![0x05];
                reply.extend((data.len() as u16).to_le_bytes());
                reply.extend(data.as_bytes());
                browser.send_to(&reply, from).await.unwrap();
            });
            let config = mssql_config(&server_of(
                "Server=tcp:127.0.0.1\\SQLEXPRESS;Database=Relay;User ID=relay;Password=test",
            ))
            .unwrap();
            let tcp = mssql_connect(&config).await.unwrap();
            assert_eq!(tcp.peer_addr().unwrap().port(), port);
            instance.accept().await.unwrap();
            // Without an answer, the error says what to check.
            let config = mssql_config(&server_of(
                "Server=tcp:127.0.0.1\\NOBODY;Database=Relay;User ID=relay;Password=test",
            ))
            .unwrap();
            let error = mssql_connect(&config).await.unwrap_err();
            assert!(error.contains("SQL Server Browser"), "{error}");
        });
    }
    fn server_of(connection: &str) -> Server {
        server(connection, true)
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
            let tcp = mssql_connect(&config).await.unwrap();
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
