//! The RFB protocol VNC speaks: the opening handshake with its security types, and the
//! messages each side sends afterwards. Versions 3.3, 3.7 and 3.8 are understood, including
//! Apple's 3.889.
use super::{
    auth::{self, AppleChallenge},
    decode::Rect,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const NONE: u8 = 1;
const VNC: u8 = 2;
const APPLE: u8 = 30;

/// The longest text the server may send at once, such as a desktop name or copied text.
pub const MAX_TEXT: usize = 16 << 20;

/// What signing in needs from the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Needs {
    /// A VNC password.
    Password,
    /// A user name and password, for a Mac's Screen Sharing.
    Account,
}

/// What the user entered at the sign-in prompt.
#[derive(Clone, Debug, Default)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

/// Why a handshake did not lead to a desktop.
#[derive(Debug, PartialEq)]
pub enum Refusal {
    /// The server wants credentials and none were given.
    Needs(Needs),
    /// The server rejected the credentials, with its reason if it gave one.
    Rejected(Needs, String),
    Failed(String),
}

impl From<std::io::Error> for Refusal {
    fn from(e: std::io::Error) -> Self {
        Self::Failed(match e.kind() {
            std::io::ErrorKind::UnexpectedEof => "the server closed the connection".into(),
            _ => e.to_string(),
        })
    }
}

/// A connected server, ready for updates.
#[derive(Debug, PartialEq)]
pub struct Server {
    pub width: u16,
    pub height: u16,
    pub name: String,
    /// Offered Apple's sign-in, so it is a Mac: Cmd is sent as the Mac's Command key.
    pub mac: bool,
}

/// Runs the handshake on `stream` with `credentials`, if the user has entered any, and sets up
/// the pixel format and `encodings`. `random` fills Apple sign-in's private key.
pub async fn handshake<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    credentials: Option<&Credentials>,
    encodings: &[i32],
    random: impl Fn(&mut [u8]) -> Result<(), String>,
) -> Result<Server, Refusal> {
    let mut version = [0u8; 12];
    stream.read_exact(&mut version).await?;
    let minor = parse_version(&version)?;
    // 3.8 is the newest version there is; Apple's 3.889 behaves as 3.8 here.
    let minor = match minor {
        8.. => 8,
        7 => 7,
        _ => 3,
    };
    stream
        .write_all(format!("RFB 003.00{minor}\n").as_bytes())
        .await?;

    let offered = if minor == 3 {
        match stream.read_u32().await? {
            0 => return Err(Refusal::Failed(reason(stream).await?)),
            kind => vec![u8::try_from(kind).unwrap_or(0)],
        }
    } else {
        let count = stream.read_u8().await?;
        if count == 0 {
            return Err(Refusal::Failed(reason(stream).await?));
        }
        let mut kinds = vec![0u8; count.into()];
        stream.read_exact(&mut kinds).await?;
        kinds
    };
    let mac = offered.contains(&APPLE);
    let Some(kind) = [NONE, APPLE, VNC].into_iter().find(|k| offered.contains(k)) else {
        return Err(Refusal::Failed(unsupported(&offered)));
    };
    let needs = match kind {
        APPLE => Some(Needs::Account),
        VNC => Some(Needs::Password),
        _ => None,
    };
    let credentials = match (&needs, credentials) {
        (Some(needs), None) => return Err(Refusal::Needs(needs.clone())),
        (_, credentials) => credentials.cloned().unwrap_or_default(),
    };
    if minor >= 7 {
        stream.write_u8(kind).await?;
    }
    match kind {
        VNC => {
            let mut challenge = [0u8; 16];
            stream.read_exact(&mut challenge).await?;
            let response = auth::vnc_response(&credentials.password, &challenge);
            stream.write_all(&response).await?;
        }
        APPLE => {
            let generator = stream.read_u16().await?;
            let length = usize::from(stream.read_u16().await?);
            let mut prime = vec![0u8; length];
            stream.read_exact(&mut prime).await?;
            let mut server_key = vec![0u8; length];
            stream.read_exact(&mut server_key).await?;
            let challenge = AppleChallenge {
                generator,
                prime,
                server_key,
            };
            let reply = auth::apple_response(
                &challenge,
                &credentials.username,
                &credentials.password,
                random,
            )
            .map_err(|e| Refusal::Rejected(Needs::Account, e))?;
            stream.write_all(&reply).await?;
        }
        _ => {}
    }
    // Before 3.8 an unauthenticated connection has no result to read.
    if minor == 8 || needs.is_some() {
        match stream.read_u32().await? {
            0 => {}
            _ => {
                let why = if minor == 8 {
                    reason(stream).await.unwrap_or_default()
                } else {
                    String::new()
                };
                let needs = needs.unwrap_or(Needs::Password);
                return Err(Refusal::Rejected(needs, why));
            }
        }
    }

    // Share the desktop with anyone else connected, rather than disconnecting them.
    stream.write_u8(1).await?;
    let width = stream.read_u16().await?;
    let height = stream.read_u16().await?;
    let mut format = [0u8; 16];
    stream.read_exact(&mut format).await?;
    let name = text(stream).await?;
    crate::frame::valid_size(width, height).map_err(Refusal::Failed)?;

    let mut setup = set_pixel_format();
    setup.extend(set_encodings(encodings));
    setup.extend(update_request(
        false,
        Rect {
            x: 0,
            y: 0,
            width,
            height,
        },
    ));
    stream.write_all(&setup).await?;
    stream.flush().await?;
    Ok(Server {
        width,
        height,
        name,
        mac,
    })
}

/// The minor version of the server's `RFB 003.00x\n` greeting.
fn parse_version(greeting: &[u8; 12]) -> Result<u16, Refusal> {
    let not_vnc = || Refusal::Failed("the server does not speak VNC".into());
    let text = std::str::from_utf8(greeting).map_err(|_| not_vnc())?;
    let rest = text.strip_prefix("RFB ").ok_or_else(not_vnc)?;
    let (major, minor) = rest
        .trim_end_matches('\n')
        .split_once('.')
        .ok_or_else(not_vnc)?;
    let major: u16 = major.parse().map_err(|_| not_vnc())?;
    let minor: u16 = minor.parse().map_err(|_| not_vnc())?;
    if major != 3 {
        return Err(Refusal::Failed(format!(
            "the server speaks RFB version {major}, and Relay understands version 3"
        )));
    }
    Ok(minor)
}

/// Why the server refused, as it says after a failure.
async fn reason<S: AsyncRead + Unpin>(stream: &mut S) -> Result<String, Refusal> {
    let reason = text(stream).await?;
    Ok(if reason.is_empty() {
        "the server refused the connection".into()
    } else {
        reason
    })
}

/// A length-prefixed string. It should be Latin-1, though many servers send UTF-8.
pub async fn text<S: AsyncRead + Unpin>(stream: &mut S) -> Result<String, Refusal> {
    let length = stream.read_u32().await? as usize;
    if length > MAX_TEXT {
        return Err(Refusal::Failed("the server sent oversized text".into()));
    }
    let mut bytes = vec![0u8; length];
    stream.read_exact(&mut bytes).await?;
    Ok(latin1_or_utf8(bytes))
}

pub fn latin1_or_utf8(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes)
        .unwrap_or_else(|e| e.into_bytes().iter().map(|&b| char::from(b)).collect())
}

/// Names the security types the server offered, none of which Relay supports.
fn unsupported(offered: &[u8]) -> String {
    let name = |kind: u8| match kind {
        5 | 6 => "RealVNC's RA2 encryption".to_string(),
        16 => "TightVNC's security".to_string(),
        18 | 19 => "VeNCrypt (TLS)".to_string(),
        kind => format!("security type {kind}"),
    };
    let mut names: Vec<String> = offered.iter().map(|&k| name(k)).collect();
    names.dedup();
    format!(
        "the server asks for {}, which Relay does not support. Relay signs in with a VNC \
         password, a Mac account, or no password",
        names.join(", ")
    )
}

/// Asks for 32-bit pixels: depth 24, little endian, true colour, red, green and blue in the
/// bytes at shifts 16, 8 and 0.
fn set_pixel_format() -> Vec<u8> {
    let mut message = vec![0, 0, 0, 0];
    message.extend([32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]);
    message
}

fn set_encodings(encodings: &[i32]) -> Vec<u8> {
    let mut message = vec![2, 0];
    message.extend((encodings.len() as u16).to_be_bytes());
    for encoding in encodings {
        message.extend(encoding.to_be_bytes());
    }
    message
}

pub fn update_request(incremental: bool, rect: Rect) -> Vec<u8> {
    let mut message = vec![3, u8::from(incremental)];
    for v in [rect.x, rect.y, rect.width, rect.height] {
        message.extend(v.to_be_bytes());
    }
    message
}

pub fn key_event(down: bool, keysym: u32) -> Vec<u8> {
    let mut message = vec![4, u8::from(down), 0, 0];
    message.extend(keysym.to_be_bytes());
    message
}

pub fn pointer_event(buttons: u8, [x, y]: [u16; 2]) -> Vec<u8> {
    let mut message = vec![5, buttons];
    message.extend(x.to_be_bytes());
    message.extend(y.to_be_bytes());
    message
}

/// Copied text for the server, in Latin-1 with line feeds as the protocol has it. Characters
/// Latin-1 lacks become question marks.
pub fn cut_text(text: &str) -> Vec<u8> {
    let latin1: Vec<u8> = text
        .replace("\r\n", "\n")
        .chars()
        .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?'))
        .collect();
    let mut message = vec![6, 0, 0, 0];
    message.extend((latin1.len() as u32).to_be_bytes());
    message.extend(latin1);
    message
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vnc::decode;
    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream, duplex};

    fn no_random(buf: &mut [u8]) -> Result<(), String> {
        buf.fill(7);
        Ok(())
    }

    /// The end of a handshake a server sends: a 1280×800 screen named `name`.
    fn server_init(name: &str) -> Vec<u8> {
        let mut init = vec![0x05, 0x00, 0x03, 0x20];
        init.extend([32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]);
        init.extend((name.len() as u32).to_be_bytes());
        init.extend(name.as_bytes());
        init
    }

    async fn read(server: &mut DuplexStream, n: usize) -> Vec<u8> {
        let mut buf = vec![0u8; n];
        server.read_exact(&mut buf).await.unwrap();
        buf
    }

    /// Reads the client's pixel format, encodings and first update request.
    async fn read_setup(server: &mut DuplexStream) {
        let format = read(server, 20).await;
        assert_eq!(format[0], 0);
        assert_eq!(&format[4..8], &[32, 24, 0, 1]);
        let head = read(server, 4).await;
        assert_eq!(head[0], 2);
        let count = u16::from_be_bytes([head[2], head[3]]) as usize;
        read(server, count * 4).await;
        assert_eq!(read(server, 10).await, [3, 0, 0, 0, 0, 0, 5, 0, 3, 32]);
    }

    #[tokio::test]
    async fn version_3_8_with_a_vnc_password() {
        let (mut client, mut server) = duplex(4096);
        let credentials = Credentials {
            username: String::new(),
            password: "password".into(),
        };
        let serve = async {
            server.write_all(b"RFB 003.008\n").await.unwrap();
            assert_eq!(read(&mut server, 12).await, b"RFB 003.008\n");
            server.write_all(&[2, 16, 2]).await.unwrap();
            assert_eq!(read(&mut server, 1).await, [2]);
            server.write_all(b"0123456789abcdef").await.unwrap();
            let response = read(&mut server, 16).await;
            assert_eq!(
                response,
                auth::vnc_response("password", b"0123456789abcdef")
            );
            server.write_all(&[0, 0, 0, 0]).await.unwrap();
            assert_eq!(read(&mut server, 1).await, [1], "shared");
            server.write_all(&server_init("desk:1")).await.unwrap();
            read_setup(&mut server).await;
        };
        let (result, ()) = tokio::join!(
            handshake(
                &mut client,
                Some(&credentials),
                &decode::ENCODINGS,
                no_random
            ),
            serve
        );
        assert_eq!(
            result.unwrap(),
            Server {
                width: 1280,
                height: 800,
                name: "desk:1".into(),
                mac: false
            }
        );
    }

    #[tokio::test]
    async fn a_password_is_asked_for_before_anything_is_sent() {
        let (mut client, mut server) = duplex(4096);
        server.write_all(b"RFB 003.008\n").await.unwrap();
        server.write_all(&[2, 30, 2]).await.unwrap();
        let result = handshake(&mut client, None, &decode::ENCODINGS, no_random).await;
        assert_eq!(result, Err(Refusal::Needs(Needs::Account)));
        // Only the version went out.
        drop(client);
        let mut sent = vec![];
        server.read_to_end(&mut sent).await.unwrap();
        assert_eq!(sent, b"RFB 003.008\n");
    }

    #[tokio::test]
    async fn rejected_passwords_carry_the_servers_reason() {
        let (mut client, mut server) = duplex(4096);
        let credentials = Credentials::default();
        let serve = async {
            server.write_all(b"RFB 003.008\n").await.unwrap();
            read(&mut server, 12).await;
            server.write_all(&[1, 2]).await.unwrap();
            read(&mut server, 1).await;
            server.write_all(&[0; 16]).await.unwrap();
            read(&mut server, 16).await;
            let why = b"Authentication failed";
            server.write_all(&[0, 0, 0, 1]).await.unwrap();
            server
                .write_all(&(why.len() as u32).to_be_bytes())
                .await
                .unwrap();
            server.write_all(why).await.unwrap();
        };
        let (result, ()) = tokio::join!(
            handshake(
                &mut client,
                Some(&credentials),
                &decode::ENCODINGS,
                no_random
            ),
            serve
        );
        assert_eq!(
            result,
            Err(Refusal::Rejected(
                Needs::Password,
                "Authentication failed".into()
            ))
        );
    }

    #[tokio::test]
    async fn version_3_3_without_a_password() {
        let (mut client, mut server) = duplex(4096);
        let serve = async {
            server.write_all(b"RFB 003.003\n").await.unwrap();
            assert_eq!(read(&mut server, 12).await, b"RFB 003.003\n");
            // The server decides: no authentication, and no result follows.
            server.write_all(&[0, 0, 0, 1]).await.unwrap();
            assert_eq!(read(&mut server, 1).await, [1]);
            server.write_all(&server_init("pi")).await.unwrap();
            read_setup(&mut server).await;
        };
        let (result, ()) = tokio::join!(
            handshake(&mut client, None, &decode::ENCODINGS, no_random),
            serve
        );
        assert_eq!(result.unwrap().name, "pi");
    }

    #[tokio::test]
    async fn apples_version_signs_in_with_an_account() {
        let (mut client, mut server) = duplex(8192);
        let credentials = Credentials {
            username: "alex".into(),
            password: "pw".into(),
        };
        let serve = async {
            server.write_all(b"RFB 003.889\n").await.unwrap();
            assert_eq!(read(&mut server, 12).await, b"RFB 003.008\n");
            server.write_all(&[3, 30, 2, 35]).await.unwrap();
            assert_eq!(read(&mut server, 1).await, [30]);
            // Generator 2, a 4-byte prime, and the server's key.
            server
                .write_all(&[0, 2, 0, 4, 0xFF, 0xFF, 0xFF, 0xFB, 0, 0, 0, 9])
                .await
                .unwrap();
            assert_eq!(read(&mut server, 128 + 4).await.len(), 132);
            server.write_all(&[0, 0, 0, 0]).await.unwrap();
            read(&mut server, 1).await;
            server.write_all(&server_init("Alex’s Mac")).await.unwrap();
            read_setup(&mut server).await;
        };
        let (result, ()) = tokio::join!(
            handshake(
                &mut client,
                Some(&credentials),
                &decode::ENCODINGS,
                no_random
            ),
            serve
        );
        let server = result.unwrap();
        assert!(server.mac);
        assert_eq!(server.name, "Alex’s Mac");
    }

    #[tokio::test]
    async fn unsupported_security_and_other_protocols_are_explained() {
        let (mut client, mut server) = duplex(4096);
        server.write_all(b"RFB 003.008\n").await.unwrap();
        server.write_all(&[2, 19, 6]).await.unwrap();
        let Err(Refusal::Failed(message)) =
            handshake(&mut client, None, &decode::ENCODINGS, no_random).await
        else {
            panic!("expected a failure");
        };
        assert!(
            message.contains("VeNCrypt (TLS), RealVNC's RA2 encryption"),
            "{message}"
        );

        let (mut client, mut server) = duplex(4096);
        server.write_all(b"SSH-2.0-Open").await.unwrap();
        assert_eq!(
            handshake(&mut client, None, &decode::ENCODINGS, no_random).await,
            Err(Refusal::Failed("the server does not speak VNC".into()))
        );

        // A server that refuses straight away says why.
        let (mut client, mut server) = duplex(4096);
        server.write_all(b"RFB 003.008\n").await.unwrap();
        server.write_all(&[0, 0, 0, 0, 4]).await.unwrap();
        server.write_all(b"busy").await.unwrap();
        assert_eq!(
            handshake(&mut client, None, &decode::ENCODINGS, no_random).await,
            Err(Refusal::Failed("busy".into()))
        );
    }

    #[test]
    fn client_messages_are_encoded_as_the_protocol_has_them() {
        assert_eq!(key_event(true, 0xff0d), [4, 1, 0, 0, 0, 0, 0xff, 0x0d]);
        assert_eq!(pointer_event(5, [300, 2]), [5, 5, 1, 44, 0, 2]);
        assert_eq!(
            cut_text("é\r\n€"),
            [6, 0, 0, 0, 0, 0, 0, 3, 0xE9, b'\n', b'?']
        );
        assert_eq!(latin1_or_utf8(vec![0xE9]), "é");
        assert_eq!(latin1_or_utf8("€".as_bytes().to_vec()), "€");
    }
}
