//! The connection itself: a thread with its own Tokio runtime that connects, decodes the
//! screen into the shared frame, sends input, and reconnects after network drops.
use super::{
    decode::{self, Change, Cursor, Rect, Screen},
    input::Input,
    rfb::{self, Credentials, Needs, Refusal},
};
use crate::frame::{Frame, Region};
use eframe::egui;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
    sync::mpsc::{self as queue, UnboundedReceiver, UnboundedSender, unbounded_channel},
};
use tokio_util::sync::{CancellationToken, DropGuard};

/// Reconnection attempts after a connection that was up drops.
const RETRIES: u32 = 5;
/// How long connecting and signing in may take before it is given up.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// How often a hidden desktop asks for what changed.
const HIDDEN_UPDATES: Duration = Duration::from_millis(500);

/// What one connection needs, taken from the saved connection and the sign-in prompt.
#[derive(Clone)]
pub struct Settings {
    pub host: String,
    pub port: u16,
    pub credentials: Option<Credentials>,
    pub clipboard: bool,
    pub view_only: bool,
}

/// What the UI asks of the connection.
#[derive(Debug, PartialEq)]
pub enum Command {
    Input(Vec<Input>),
    /// Text copied locally, for the remote clipboard.
    Offer(String),
}

/// What the connection reports to the UI.
#[derive(Debug, PartialEq)]
pub enum Event {
    /// Signed in; `mac` when the server is a Mac.
    Connected {
        mac: bool,
    },
    /// The connection dropped and is being restored.
    Reconnecting,
    Ended(Outcome),
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// The server wants credentials, or did not accept the ones given.
    SignIn(Needs, Option<String>),
    Failed(String),
}

/// The pointer shape, and whether the UI has seen the latest.
#[derive(Default)]
pub struct Pointer {
    pub shape: Option<Cursor>,
    pub changed: bool,
}

/// A running connection, stopped when dropped.
pub struct Worker {
    commands: UnboundedSender<Command>,
    events: mpsc::Receiver<Event>,
    pub frame: Arc<Mutex<Frame>>,
    pub pointer: Arc<Mutex<Pointer>>,
    /// Text the remote computer copied, if the UI has not taken it yet.
    pub copied: Arc<Mutex<Option<String>>>,
    /// Stops the connection wherever it waits, which lets go of its socket, picture and
    /// password even while the server does not answer.
    _stop: DropGuard,
}

impl Worker {
    /// Starts connecting. Screen updates wake the window only while `shown` is set.
    pub fn start(
        settings: Settings,
        ctx: egui::Context,
        shown: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        let (commands, receiver) = unbounded_channel();
        let (events, reports) = mpsc::channel();
        let stop = CancellationToken::new();
        let stopped = stop.clone();
        let link = Link {
            events,
            ctx,
            frame: Arc::default(),
            pointer: Arc::default(),
            copied: Arc::default(),
            clipboard: settings.clipboard,
            shown,
        };
        let (frame, pointer, copied) = (
            link.frame.clone(),
            link.pointer.clone(),
            link.copied.clone(),
        );
        std::thread::Builder::new()
            .name("vnc".into())
            .spawn(move || {
                match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime.block_on(async {
                        tokio::select! {
                            biased;
                            () = stopped.cancelled() => {}
                            () = run(settings, receiver, &link) => {}
                        }
                    }),
                    Err(e) => link.send(Event::Ended(Outcome::Failed(format!(
                        "Cannot start the connection: {e}"
                    )))),
                }
            })
            .map_err(|e| format!("Cannot start the connection: {e}"))?;
        Ok(Self {
            commands,
            events: reports,
            frame,
            pointer,
            copied,
            _stop: stop.drop_guard(),
        })
    }

    pub fn send(&self, command: Command) {
        // A connection that has ended no longer listens; its end is reported as an event.
        let _ = self.commands.send(command);
    }

    pub fn events(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }
}

/// The connection thread's way back to the UI.
#[derive(Clone)]
struct Link {
    events: mpsc::Sender<Event>,
    ctx: egui::Context,
    frame: Arc<Mutex<Frame>>,
    pointer: Arc<Mutex<Pointer>>,
    /// Only the latest copied text is kept, however much the server sends before the UI
    /// takes it.
    copied: Arc<Mutex<Option<String>>>,
    /// Whether copied text is shared with the local clipboard.
    clipboard: bool,
    /// Whether the desktop is on screen. A hidden one asks for updates less often and does
    /// not wake the window, and shows its latest picture when its tab does.
    shown: Arc<AtomicBool>,
}

impl Link {
    fn send(&self, event: Event) {
        let _ = self.events.send(event);
        self.ctx.request_repaint();
    }
    fn shown(&self) -> bool {
        self.shown.load(Ordering::Relaxed)
    }
}

async fn run(settings: Settings, mut commands: UnboundedReceiver<Command>, link: &Link) {
    let mut retries = 0;
    let mut was_up = false;
    loop {
        let attempt = tokio::time::timeout(CONNECT_TIMEOUT, connect(&settings, link))
            .await
            .unwrap_or_else(|_| Err(Refusal::Failed("the server stopped answering".into())));
        let error = match attempt {
            Ok((stream, server)) => {
                link.send(Event::Connected { mac: server.mac });
                was_up = true;
                retries = 0;
                match session(stream, server, &settings, &mut commands, link).await {
                    None => return,
                    Some(error) => error,
                }
            }
            Err(Refusal::Needs(needs)) => {
                link.send(Event::Ended(Outcome::SignIn(needs, None)));
                return;
            }
            Err(Refusal::Rejected(needs, reason)) => {
                link.send(Event::Ended(Outcome::SignIn(
                    needs.clone(),
                    Some(rejected(&needs, &reason)),
                )));
                return;
            }
            // Only a session that was up reconnects by itself; anything else needs the user.
            Err(Refusal::Failed(error)) if !was_up => {
                link.send(Event::Ended(Outcome::Failed(format!(
                    "Cannot connect to {}: {error}.",
                    settings.host
                ))));
                return;
            }
            Err(Refusal::Failed(error)) => error,
        };
        if retries >= RETRIES || commands.is_closed() {
            link.send(Event::Ended(Outcome::Failed(format!(
                "The connection to {} was lost: {error}.",
                settings.host
            ))));
            return;
        }
        retries += 1;
        link.send(Event::Reconnecting);
        tokio::time::sleep(Duration::from_secs(2 << retries.min(4))).await;
    }
}

/// What to tell the user when the server did not accept their credentials.
fn rejected(needs: &Needs, reason: &str) -> String {
    let message = match needs {
        Needs::Password => "The password was not accepted.",
        Needs::Account => "The user name or password was not accepted.",
    };
    let generic = ["", "authentication failed", "password check failed"];
    if generic.contains(
        &reason
            .trim()
            .trim_end_matches(['.', '!'])
            .to_lowercase()
            .as_str(),
    ) {
        message.into()
    } else {
        format!("{message} The server said: {}", reason.trim())
    }
}

/// Connects and signs in, leaving the server ready to send its screen.
async fn connect(settings: &Settings, link: &Link) -> Result<(TcpStream, rfb::Server), Refusal> {
    let mut stream = TcpStream::connect((settings.host.as_str(), settings.port)).await?;
    // Keys and pointer moves are small; sending each at once keeps typing responsive.
    stream.set_nodelay(true)?;
    let random = |buf: &mut [u8]| getrandom::fill(buf).map_err(|e| e.to_string());
    // Without input there is no local pointer to draw, so a server that can draws its own.
    let encodings: Vec<i32> = (decode::ENCODINGS.iter().copied())
        .filter(|&e| !settings.view_only || e != decode::CURSOR)
        .collect();
    let server = rfb::handshake(
        &mut stream,
        settings.credentials.as_ref(),
        &encodings,
        random,
    )
    .await?;
    if let Ok(mut frame) = link.frame.lock() {
        frame.resize(server.width.into(), server.height.into());
    }
    if let Ok(mut pointer) = link.pointer.lock() {
        *pointer = Pointer {
            shape: None,
            changed: true,
        };
    }
    link.ctx.request_repaint();
    Ok((stream, server))
}

/// What the reader task reports to the session loop.
enum Incoming {
    /// A whole screen update was applied to a screen of this size, so the next can be asked
    /// for.
    Updated([u16; 2]),
    Failed(String),
}

/// Runs a connected session until the UI closes it, returning `None`, or the connection
/// fails, returning why.
async fn session(
    stream: TcpStream,
    server: rfb::Server,
    settings: &Settings,
    commands: &mut UnboundedReceiver<Command>,
    link: &Link,
) -> Option<String> {
    let (reader, mut writer) = stream.into_split();
    // Bounded, so a server that stops reading while it keeps sending cannot pile up updates
    // behind a blocked write; the reader waits instead.
    let (incoming, mut updates) = queue::channel(1);
    let screen = Screen::new(server.width, server.height);
    let receiving = link.clone();
    // Reading runs on its own so that a half-read update is never abandoned for a key press.
    let reader = tokio::spawn(async move {
        let mut reader = BufReader::with_capacity(1 << 16, reader);
        let error = receive(&mut reader, screen, &receiving, &incoming).await;
        let _ = incoming.send(Incoming::Failed(error)).await;
    });
    let _reader = AbortOnDrop(reader);
    let mut size = [server.width, server.height];
    // The first update was asked for during the handshake.
    let mut deferred = false;
    let mut ticks = tokio::time::interval(HIDDEN_UPDATES);
    loop {
        let message = tokio::select! {
            update = updates.recv() => match update {
                Some(Incoming::Updated(now)) => {
                    size = now;
                    if link.shown() {
                        Some(next_update(size))
                    } else {
                        deferred = true;
                        None
                    }
                }
                Some(Incoming::Failed(error)) => return Some(error),
                None => return Some("the connection ended".into()),
            },
            command = commands.recv() => match command {
                None => return None,
                Some(_) if settings.view_only => None,
                Some(Command::Input(input)) => Some(input.iter().flat_map(encode).collect()),
                Some(Command::Offer(text)) => settings.clipboard.then(|| rfb::cut_text(&text)),
            },
            _ = ticks.tick(), if deferred => {
                (link.shown()).then(|| {
                    deferred = false;
                    next_update(size)
                })
            }
        };
        if let Some(message) = message
            && let Err(e) = writer.write_all(&message).await
        {
            return Some(e.to_string());
        }
    }
}

/// Asks for whatever changed on a screen of `size`.
fn next_update([width, height]: [u16; 2]) -> Vec<u8> {
    rfb::update_request(
        true,
        Rect {
            x: 0,
            y: 0,
            width,
            height,
        },
    )
}

fn encode(input: &Input) -> Vec<u8> {
    match *input {
        Input::Key { down, keysym } => rfb::key_event(down, keysym),
        Input::Pointer { buttons, at } => rfb::pointer_event(buttons, at),
    }
}

/// Stops the reader task with the session, wherever it waits.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Reads server messages until the connection fails, returning why.
async fn receive<R: AsyncRead + Unpin>(
    reader: &mut R,
    mut screen: Screen,
    link: &Link,
    incoming: &queue::Sender<Incoming>,
) -> String {
    loop {
        if let Err(error) = message(reader, &mut screen, link, incoming).await {
            return error;
        }
    }
}

async fn message<R: AsyncRead + Unpin>(
    reader: &mut R,
    screen: &mut Screen,
    link: &Link,
    incoming: &queue::Sender<Incoming>,
) -> Result<(), String> {
    let io = |e: std::io::Error| match e.kind() {
        std::io::ErrorKind::UnexpectedEof => "the server closed the connection".to_string(),
        _ => e.to_string(),
    };
    match reader.read_u8().await.map_err(io)? {
        // A screen update: rectangles, each in an encoding.
        0 => {
            reader.read_u8().await.map_err(io)?;
            let count = reader.read_u16().await.map_err(io)?;
            for _ in 0..count {
                let mut header = [0u8; 12];
                reader.read_exact(&mut header).await.map_err(io)?;
                let field = |i: usize| u16::from_be_bytes([header[i], header[i + 1]]);
                let rect = Rect {
                    x: field(0),
                    y: field(2),
                    width: field(4),
                    height: field(6),
                };
                let encoding = i32::from_be_bytes([header[8], header[9], header[10], header[11]]);
                match screen.apply(reader, rect, encoding).await? {
                    Change::Pixels(rect) => {
                        if let Ok(mut frame) = link.frame.lock() {
                            frame.copy(
                                &screen.rgba,
                                Region {
                                    left: rect.x.into(),
                                    top: rect.y.into(),
                                    right: usize::from(rect.x) + usize::from(rect.width),
                                    bottom: usize::from(rect.y) + usize::from(rect.height),
                                },
                            );
                        }
                    }
                    Change::Resized => {
                        if let Ok(mut frame) = link.frame.lock() {
                            frame.resize(screen.width, screen.height);
                        }
                    }
                    Change::Cursor(cursor) => {
                        if let Ok(mut pointer) = link.pointer.lock() {
                            *pointer = Pointer {
                                shape: Some(cursor),
                                changed: true,
                            };
                        }
                    }
                    Change::Nothing => {}
                }
            }
            if link.shown() {
                link.ctx.request_repaint();
            }
            // Sides are at most `decode::MAX_SIDE`, so they fit.
            let size = [screen.width, screen.height].map(|v| v as u16);
            let _ = incoming.send(Incoming::Updated(size)).await;
        }
        // Colour map entries, which true colour does not use.
        1 => {
            let mut head = [0u8; 5];
            reader.read_exact(&mut head).await.map_err(io)?;
            let count = usize::from(u16::from_be_bytes([head[3], head[4]]));
            let mut skip = vec![0u8; count * 6];
            reader.read_exact(&mut skip).await.map_err(io)?;
        }
        // The bell.
        2 => {}
        3 => {
            let mut padding = [0u8; 3];
            reader.read_exact(&mut padding).await.map_err(io)?;
            let text = rfb::text(reader).await.map_err(|e| match e {
                Refusal::Failed(e) => e,
                _ => "the server sent unreadable text".into(),
            })?;
            if link.clipboard
                && let Ok(mut copied) = link.copied.lock()
            {
                *copied = Some(text);
                link.ctx.request_repaint();
            }
        }
        kind => {
            return Err(format!(
                "the server sent a message Relay does not know ({kind})"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    fn settings(port: u16) -> Settings {
        Settings {
            host: "127.0.0.1".into(),
            port,
            credentials: None,
            clipboard: true,
            view_only: false,
        }
    }

    /// Waits for the worker's next event.
    fn next(worker: &Worker) -> Event {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(event) = worker.events.try_iter().next() {
                return event;
            }
            assert!(std::time::Instant::now() < deadline, "no event arrived");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Accepts one connection and runs the server side of a handshake without a password,
    /// for a 4×2 screen.
    async fn accept(listener: &tokio::net::TcpListener) -> TcpStream {
        let (mut socket, _) = listener.accept().await.unwrap();
        socket.write_all(b"RFB 003.008\n").await.unwrap();
        let mut buf = [0u8; 12];
        socket.read_exact(&mut buf).await.unwrap();
        socket.write_all(&[1, 1]).await.unwrap();
        let mut buf = [0u8; 1];
        socket.read_exact(&mut buf).await.unwrap();
        socket.write_all(&[0, 0, 0, 0]).await.unwrap();
        socket.read_exact(&mut buf).await.unwrap();
        let mut init = vec![0, 4, 0, 2];
        init.extend([32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]);
        init.extend([0, 0, 0, 0]);
        socket.write_all(&init).await.unwrap();
        // Pixel format, encodings and the first update request.
        let mut head = [0u8; 24];
        socket.read_exact(&mut head).await.unwrap();
        let count = usize::from(u16::from_be_bytes([head[22], head[23]]));
        let mut rest = vec![0u8; count * 4 + 10];
        socket.read_exact(&mut rest).await.unwrap();
        socket
    }

    #[test]
    fn a_session_shows_updates_takes_input_and_shares_the_clipboard() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let worker = Worker::start(
            settings(port),
            egui::Context::default(),
            Arc::new(AtomicBool::new(true)),
        )
        .unwrap();
        let mut socket = runtime.block_on(accept(&listener));
        assert_eq!(next(&worker), Event::Connected { mac: false });
        runtime.block_on(async {
            // One raw red pixel at 1,0, then copied text.
            let mut update = vec![0, 0, 0, 1, 0, 1, 0, 0, 0, 1, 0, 1, 0, 0, 0, 0];
            update.extend([0, 0, 255, 0]);
            update.extend([3, 0, 0, 0, 0, 0, 0, 3, b'a', b'\n', b'b']);
            socket.write_all(&update).await.unwrap();
            // The client asks for the next update.
            let mut request = [0u8; 10];
            socket.read_exact(&mut request).await.unwrap();
            assert_eq!(request, [3, 1, 0, 0, 0, 0, 0, 4, 0, 2]);
        });
        // The text may land just after the request goes out.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while worker.copied.lock().unwrap().is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "no copied text arrived"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            worker.copied.lock().unwrap().take().as_deref(),
            Some("a\nb")
        );
        let update = worker.frame.lock().unwrap().take();
        let Some(crate::frame::Update::Full(image)) = update else {
            panic!("expected the first picture");
        };
        assert_eq!(image.pixels[1], egui::Color32::from_rgb(255, 0, 0));
        worker.send(Command::Input(vec![Input::Key {
            down: true,
            keysym: 0x61,
        }]));
        worker.send(Command::Offer("é".into()));
        runtime.block_on(async {
            let mut sent = [0u8; 8 + 9];
            socket.read_exact(&mut sent).await.unwrap();
            assert_eq!(&sent[..8], &rfb::key_event(true, 0x61)[..]);
            assert_eq!(&sent[8..], &rfb::cut_text("é")[..]);
        });
        // Closing the tab ends the connection.
        drop(worker);
        runtime.block_on(async {
            let mut rest = vec![];
            tokio::time::timeout(Duration::from_secs(5), socket.read_to_end(&mut rest))
                .await
                .expect("the connection stayed open")
                .unwrap();
        });
    }

    #[test]
    fn a_password_prompt_ends_the_attempt() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let worker = Worker::start(
            settings(port),
            egui::Context::default(),
            Arc::new(AtomicBool::new(true)),
        )
        .unwrap();
        runtime.block_on(async {
            let (mut socket, _) = listener.accept().await.unwrap();
            socket.write_all(b"RFB 003.008\n\x01\x02").await.unwrap();
            let mut rest = vec![];
            socket.read_to_end(&mut rest).await.unwrap();
        });
        assert_eq!(
            next(&worker),
            Event::Ended(Outcome::SignIn(Needs::Password, None))
        );
    }

    #[test]
    fn view_only_sessions_send_nothing() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let worker = Worker::start(
            Settings {
                view_only: true,
                ..settings(port)
            },
            egui::Context::default(),
            Arc::new(AtomicBool::new(true)),
        )
        .unwrap();
        let mut socket = runtime.block_on(accept(&listener));
        assert_eq!(next(&worker), Event::Connected { mac: false });
        worker.send(Command::Input(vec![Input::Key {
            down: true,
            keysym: 0x61,
        }]));
        worker.send(Command::Offer("secret".into()));
        drop(worker);
        runtime.block_on(async {
            let mut rest = vec![];
            socket.read_to_end(&mut rest).await.unwrap();
            assert!(rest.is_empty(), "sent {rest:?}");
        });
    }

    /// Signs in to a real server, waits for its picture, then types, points and offers text.
    /// `RELAY_TEST_VNC` is `host:port`; `RELAY_TEST_VNC_USER` and `RELAY_TEST_VNC_PASSWORD`
    /// answer a sign-in prompt, and `RELAY_TEST_VNC_TYPE` is typed into the desktop.
    #[test]
    #[ignore = "needs a VNC server"]
    fn vnc_live() {
        let target = std::env::var("RELAY_TEST_VNC").expect("RELAY_TEST_VNC");
        let (host, port) = target.rsplit_once(':').expect("host:port");
        let mut settings = Settings {
            host: host.into(),
            ..settings(port.parse().expect("a port"))
        };
        let shown = Arc::new(AtomicBool::new(true));
        let mut worker =
            Worker::start(settings.clone(), egui::Context::default(), shown.clone()).unwrap();
        let mut event = next(&worker);
        if let Event::Ended(Outcome::SignIn(needs, None)) = &event {
            settings.credentials = Some(Credentials {
                username: std::env::var("RELAY_TEST_VNC_USER").unwrap_or_default(),
                password: std::env::var("RELAY_TEST_VNC_PASSWORD")
                    .expect("the server asks for a password"),
            });
            eprintln!("signing in for {needs:?}");
            worker = Worker::start(settings, egui::Context::default(), shown).unwrap();
            event = next(&worker);
        }
        let Event::Connected { mac } = event else {
            panic!("did not connect: {event:?}");
        };
        eprintln!("connected, mac: {mac}");
        // Updates arrive until something other than black shows.
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let mut picture: Option<egui::ColorImage> = None;
        loop {
            match worker.frame.lock().unwrap().take() {
                Some(crate::frame::Update::Full(image)) => picture = Some(image),
                Some(crate::frame::Update::Partial([x, y], image)) => {
                    let whole = picture.as_mut().expect("a full picture first");
                    for row in 0..image.size[1] {
                        for column in 0..image.size[0] {
                            whole.pixels[(y + row) * whole.size[0] + x + column] =
                                image.pixels[row * image.size[0] + column];
                        }
                    }
                }
                None => {}
            }
            if (picture.as_ref())
                .is_some_and(|p| p.pixels.iter().any(|c| *c != egui::Color32::BLACK))
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the picture stayed black"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        // Let the first updates settle, then keep the picture for comparing encodings.
        std::thread::sleep(Duration::from_secs(1));
        let mut picture = picture.unwrap();
        while let Some(update) = worker.frame.lock().unwrap().take() {
            if let crate::frame::Update::Partial([x, y], image) = update {
                for row in 0..image.size[1] {
                    for column in 0..image.size[0] {
                        picture.pixels[(y + row) * picture.size[0] + x + column] =
                            image.pixels[row * image.size[0] + column];
                    }
                }
            }
        }
        let size = picture.size;
        eprintln!("picture {size:?}");
        if let Ok(path) = std::env::var("RELAY_TEST_VNC_DUMP") {
            let mut ppm = format!("P6 {} {} 255\n", size[0], size[1]).into_bytes();
            for p in &picture.pixels {
                ppm.extend([p.r(), p.g(), p.b()]);
            }
            std::fs::write(path, ppm).unwrap();
        }
        let pointer = |buttons, at| Input::Pointer { buttons, at };
        worker.send(Command::Input(vec![
            pointer(0, [40, 40]),
            pointer(1, [40, 40]),
            pointer(0, [40, 40]),
        ]));
        if let Ok(text) = std::env::var("RELAY_TEST_VNC_TYPE") {
            let mut keys = vec![];
            for c in text.chars() {
                let keysym = if c == '\n' {
                    0xff0d
                } else {
                    super::super::input::keysym(c)
                };
                keys.push(Input::Key { down: true, keysym });
                keys.push(Input::Key {
                    down: false,
                    keysym,
                });
            }
            worker.send(Command::Input(keys));
        }
        worker.send(Command::Offer("copied by relay".into()));
        // Nothing the server says in the meantime may end the session.
        let seconds = std::env::var("RELAY_TEST_VNC_SECONDS").map_or(3, |s| s.parse().unwrap());
        for _ in 0..seconds * 10 {
            std::thread::sleep(Duration::from_millis(100));
            for event in worker.events() {
                eprintln!("{event:?}");
                assert!(!matches!(event, Event::Ended(_) | Event::Reconnecting));
            }
            if let Some(crate::frame::Update::Full(image)) = worker.frame.lock().unwrap().take() {
                eprintln!("new picture {:?}", image.size);
            }
            let mut pointer = worker.pointer.lock().unwrap();
            if std::mem::take(&mut pointer.changed) {
                let shape = pointer.shape.as_ref().map(|s| (s.size, s.hotspot));
                eprintln!("pointer shape: {shape:?}");
            }
        }
    }

    #[test]
    fn rejections_explain_themselves() {
        assert_eq!(
            rejected(&Needs::Password, ""),
            "The password was not accepted."
        );
        assert_eq!(
            rejected(&Needs::Account, "Authentication failed!"),
            "The user name or password was not accepted."
        );
        assert_eq!(
            rejected(&Needs::Password, "Too many security failures"),
            "The password was not accepted. The server said: Too many security failures"
        );
    }
}
