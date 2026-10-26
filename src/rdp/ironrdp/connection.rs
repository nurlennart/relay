//! The connection itself: a thread with its own Tokio runtime that connects, decodes the
//! desktop into the shared frame, sends input, and reconnects after network drops.
use super::{
    clipboard::{self, Backend, Sink},
    frame::{Frame, Region},
};
use eframe::egui;
use ironrdp_cliprdr::{CliprdrClient, backend::ClipboardMessage};
use ironrdp_connector::{
    ClientConnector, ConnectionResult, ConnectorError, ConnectorErrorKind, ConnectorResult,
    Credentials, DesktopSize, connection_activation::ConnectionActivationState,
};
use ironrdp_core::WriteBuf;
use ironrdp_displaycontrol::{client::DisplayControlClient, pdu::MonitorLayoutEntry};
use ironrdp_graphics::image_processing::PixelFormat;
use ironrdp_pdu::{gcc, input::fast_path::FastPathInputEvent, rdp::capability_sets};
use ironrdp_session::{ActiveStage, ActiveStageBuilder, ActiveStageOutput, image::DecodedImage};
use ironrdp_tokio::{FramedWrite, TokioFramed, single_sequence_step_read, split_tokio_framed};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
};

/// Reconnection attempts after a connection that was up drops.
const RETRIES: u32 = 5;

/// What one connection needs, taken from the saved connection and the sign-in prompt.
#[derive(Clone)]
pub struct Settings {
    pub host: String,
    pub port: u16,
    /// RD Gateway host, with an optional port.
    pub gateway: Option<String>,
    pub username: String,
    pub domain: Option<String>,
    pub password: String,
    /// The initial remote resolution in pixels.
    pub size: [u16; 2],
    /// The remote display scale in percent.
    pub scale: u32,
    pub clipboard: bool,
}

/// What the UI asks of the connection.
pub enum Command {
    Input(Vec<FastPathInputEvent>),
    /// A new remote resolution, in pixels, and display scale in percent.
    Resize([u16; 2], u32),
    /// Text copied locally, to offer the remote computer.
    Offer(String),
}

/// What the connection reports to the UI.
#[derive(Debug, PartialEq)]
pub enum Event {
    Connected,
    /// The connection dropped and is being restored.
    Reconnecting,
    /// Text the remote computer copied.
    Copied(String),
    /// The remote computer cannot change its resolution, so the picture is scaled instead.
    FixedResolution,
    Ended(Outcome),
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// The remote side ended the session, for example by signing out.
    Closed(String),
    /// The user name or password was not accepted.
    SignIn(String),
    /// A certificate failed validation and the user has not trusted it.
    Certificate(Untrusted),
    Failed(String),
}

/// A server certificate that failed validation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Untrusted {
    /// The host it was presented for: the remote computer or the gateway.
    pub host: String,
    /// The SHA-256 fingerprint of the certificate.
    pub fingerprint: [u8; 32],
    pub reason: String,
}

impl Untrusted {
    /// The fingerprint as colon-separated hex, as certificate viewers show it.
    pub fn fingerprint_text(&self) -> String {
        let hex: Vec<String> = self
            .fingerprint
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect();
        hex.join(":")
    }
}

/// Certificates the user accepted, by host and fingerprint, until Relay quits.
type Trusted = HashSet<(String, [u8; 32])>;
static TRUSTED: Mutex<Option<Trusted>> = Mutex::new(None);

/// Accepts `certificate` for its host for the rest of this run.
pub fn trust(certificate: &Untrusted) {
    if let Ok(mut trusted) = TRUSTED.lock() {
        trusted
            .get_or_insert_with(HashSet::new)
            .insert((certificate.host.clone(), certificate.fingerprint));
    }
}

fn is_trusted(host: &str, fingerprint: &[u8; 32]) -> bool {
    TRUSTED.lock().is_ok_and(|trusted| {
        trusted
            .as_ref()
            .is_some_and(|t| t.contains(&(host.to_owned(), *fingerprint)))
    })
}

/// A running connection, stopped when dropped.
pub struct Worker {
    commands: UnboundedSender<Command>,
    events: mpsc::Receiver<Event>,
    pub frame: Arc<Mutex<Frame>>,
}

impl Worker {
    pub fn start(settings: Settings, ctx: egui::Context) -> Result<Self, String> {
        let (commands, receiver) = unbounded_channel();
        let (events, reports) = mpsc::channel();
        let frame = Arc::new(Mutex::new(Frame::default()));
        let link = Link {
            events,
            ctx,
            frame: frame.clone(),
        };
        std::thread::Builder::new()
            .name("rdp".into())
            .spawn(move || {
                match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime.block_on(run(settings, receiver, &link)),
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
struct Link {
    events: mpsc::Sender<Event>,
    ctx: egui::Context,
    frame: Arc<Mutex<Frame>>,
}

impl Link {
    fn send(&self, event: Event) {
        let _ = self.events.send(event);
        self.ctx.request_repaint();
    }
}

/// Clipboard messages, from the backend into the session loop, and copied text to the UI.
#[derive(Debug)]
struct ClipboardSink {
    messages: UnboundedSender<ClipboardMessage>,
    events: mpsc::Sender<Event>,
    ctx: egui::Context,
}

impl Sink for ClipboardSink {
    fn send(&self, message: ClipboardMessage) {
        let _ = self.messages.send(message);
    }
    fn copied(&self, text: String) {
        let _ = self.events.send(Event::Copied(text));
        self.ctx.request_repaint();
    }
}

/// How a session that was up stopped.
enum Stop {
    /// The UI closed the tab.
    Closed,
    Ended(String),
    Dropped(String),
}

async fn run(mut settings: Settings, mut commands: UnboundedReceiver<Command>, link: &Link) {
    let (messages, mut clipboard_messages) = unbounded_channel();
    let offered = Arc::new(Mutex::new(None));
    let mut retries = 0;
    let mut was_up = false;
    loop {
        let untrusted = Arc::new(Mutex::new(None));
        let backend = settings.clipboard.then(|| {
            Backend::new(
                ClipboardSink {
                    messages: messages.clone(),
                    events: link.events.clone(),
                    ctx: link.ctx.clone(),
                },
                offered.clone(),
            )
        });
        let error = match connect(&settings, &untrusted, backend).await {
            Ok((result, framed)) => {
                link.send(Event::Connected);
                was_up = true;
                retries = 0;
                let stop = session(
                    result,
                    framed,
                    &mut settings,
                    &mut commands,
                    &mut clipboard_messages,
                    &offered,
                    link,
                )
                .await;
                match stop {
                    Stop::Closed => return,
                    Stop::Ended(reason) => {
                        link.send(Event::Ended(Outcome::Closed(reason)));
                        return;
                    }
                    Stop::Dropped(error) => error,
                }
            }
            Err(error) => {
                let certificate = untrusted.lock().ok().and_then(|mut u| u.take());
                let outcome = classify(&error, certificate, &settings.host);
                // Only a session that was up reconnects by itself; anything else needs the user.
                if !was_up || !matches!(outcome, Outcome::Failed(_)) {
                    link.send(Event::Ended(outcome));
                    return;
                }
                error.report().to_string()
            }
        };
        if retries >= RETRIES || commands.is_closed() {
            link.send(Event::Ended(Outcome::Failed(format!(
                "The connection to {} was lost: {error}",
                settings.host
            ))));
            return;
        }
        retries += 1;
        link.send(Event::Reconnecting);
        tokio::time::sleep(Duration::from_secs(2 << retries.min(4))).await;
    }
}

/// What a failed connection attempt means for the user.
fn classify(error: &ConnectorError, certificate: Option<Untrusted>, host: &str) -> Outcome {
    if let Some(certificate) = certificate {
        return Outcome::Certificate(certificate);
    }
    match error.kind() {
        ConnectorErrorKind::Credssp(_) | ConnectorErrorKind::AccessDenied => Outcome::SignIn(
            "Sign-in failed. Check the user name and password, and that this account may sign \
             in remotely."
                .into(),
        ),
        _ => Outcome::Failed(format!("Cannot connect to {host}: {}", error.report())),
    }
}

trait Stream: AsyncRead + AsyncWrite + Unpin + Send + Sync {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send + Sync> Stream for T {}
type Framed = TokioFramed<Box<dyn Stream>>;

/// Validates certificates against the system's roots, and failing that against the ones the
/// user trusted. An untrusted certificate is recorded for the prompt and rejected.
fn verifier(
    untrusted: &Arc<Mutex<Option<Untrusted>>>,
) -> ironrdp_tls::CertificateValidationCallback {
    let untrusted = untrusted.clone();
    Arc::new(move |der: &[u8], endpoint: &str, reason: &str| {
        let fingerprint: [u8; 32] = Sha256::digest(der).into();
        if is_trusted(endpoint, &fingerprint) {
            return true;
        }
        if let Ok(mut slot) = untrusted.lock() {
            *slot = Some(Untrusted {
                host: endpoint.to_owned(),
                fingerprint,
                reason: reason.to_owned(),
            });
        }
        false
    })
}

/// The KDC client CredSSP would use for Kerberos. Relay signs in with NTLM, so it has none.
struct NoKdc;

impl ironrdp_tokio::NetworkClient for NoKdc {
    async fn send(
        &mut self,
        _: &ironrdp_connector::sspi::generator::NetworkRequest,
    ) -> ConnectorResult<Vec<u8>> {
        Err(ironrdp_connector::general_err!(
            "Kerberos sign-in is not supported"
        ))
    }
}

async fn connect(
    settings: &Settings,
    untrusted: &Arc<Mutex<Option<Untrusted>>>,
    clipboard: Option<Backend<ClipboardSink>>,
) -> ConnectorResult<(ConnectionResult, Framed)> {
    let verify = verifier(untrusted);
    let (stream, client_addr): (Box<dyn Stream>, _) = match &settings.gateway {
        None => {
            let stream = tokio::net::TcpStream::connect((settings.host.as_str(), settings.port))
                .await
                .map_err(|e| ironrdp_connector::custom_err!("TCP connect", e))?;
            let addr = stream
                .local_addr()
                .map_err(|e| ironrdp_connector::custom_err!("local address", e))?;
            (Box::new(stream), addr)
        }
        Some(gateway) => {
            let target = ironrdp_mstsgu::GwConnectTarget {
                gw_endpoint: gateway.clone(),
                gw_user: qualified_user(settings),
                gw_pass: settings.password.clone(),
                smart_card: None,
                server: settings.host.clone(),
            };
            let (stream, addr) =
                ironrdp_mstsgu::GwClient::connect_with_port_and_certificate_validation(
                    &target,
                    CLIENT_NAME,
                    settings.port,
                    ironrdp_tls::CertificateValidation::Strict,
                    Some(verify.clone()),
                )
                .await
                .map_err(|e| ironrdp_connector::custom_err!("RD Gateway", e))?;
            (Box::new(stream), addr)
        }
    };
    let mut framed = Framed::new(stream);
    let channels = ironrdp_dvc::DrdynvcClient::new()
        .with_dynamic_channel(DisplayControlClient::new(|_| Ok(Vec::new())));
    let mut connector =
        ClientConnector::new(config(settings)?, client_addr).with_static_channel(channels);
    if let Some(backend) = clipboard {
        connector.attach_static_channel(CliprdrClient::new(Box::new(backend)));
    }

    let should_upgrade = ironrdp_tokio::connect_begin(&mut framed, &mut connector).await?;
    let (stream, leftover) = framed.into_inner();
    let (tls, certificate) =
        ironrdp_tls::upgrade_with_certificate_validation_callback(stream, &settings.host, verify)
            .await
            .map_err(|e| ironrdp_connector::custom_err!("TLS", e))?;
    let upgraded = ironrdp_tokio::mark_as_upgraded(should_upgrade, &mut connector);
    let mut framed = Framed::new_with_leftover(Box::new(tls) as Box<dyn Stream>, leftover);
    let public_key = ironrdp_tls::extract_tls_server_public_key(&certificate)
        .ok_or_else(|| ironrdp_connector::general_err!("the server certificate has no public key"))?
        .to_owned();
    let result = ironrdp_tokio::connect_finalize(
        upgraded,
        connector,
        &mut framed,
        &mut NoKdc,
        settings.host.as_str().into(),
        public_key,
        None,
    )
    .await?
    .into_connected()?;
    Ok((result, framed))
}

/// How Relay names itself to the remote computer.
const CLIENT_NAME: &str = "Relay";

/// The user name with its domain, as gateways expect it.
fn qualified_user(settings: &Settings) -> String {
    match &settings.domain {
        Some(domain) => format!(r"{domain}\{}", settings.username),
        None => settings.username.clone(),
    }
}

/// The IronRDP connector configuration for `settings`.
pub fn config(settings: &Settings) -> ConnectorResult<ironrdp_connector::Config> {
    let codecs = capability_sets::client_codecs_capabilities(&[])
        .map_err(|e| ironrdp_connector::reason_err!("codecs", "{e}"))?;
    Ok(ironrdp_connector::Config {
        desktop_size: DesktopSize {
            width: settings.size[0],
            height: settings.size[1],
        },
        monitor_layout: None,
        desktop_scale_factor: settings.scale,
        // NLA is preferred; servers without it, such as many xrdp hosts, still work over TLS.
        enable_tls: true,
        enable_credssp: true,
        enable_standard_rdp_security: false,
        credentials: Credentials::UsernamePassword {
            username: settings.username.clone(),
            password: settings.password.clone(),
        },
        domain: settings.domain.clone(),
        client_build: 0,
        client_name: CLIENT_NAME.into(),
        keyboard_type: gcc::KeyboardType::IBM_ENHANCED,
        keyboard_subtype: 0,
        keyboard_functional_keys_count: 12,
        // Zero lets the remote computer use its own keyboard layout.
        keyboard_layout: 0,
        connection_type: gcc::ConnectionType::Autodetect,
        ime_file_name: String::new(),
        bitmap: Some(ironrdp_connector::BitmapConfig {
            lossy_compression: true,
            color_depth: 32,
            codecs,
        }),
        dig_product_id: String::new(),
        client_dir: String::new(),
        alternate_shell: String::new(),
        work_dir: String::new(),
        remote_application_mode: false,
        rail_support_level: capability_sets::RailSupportLevel::empty(),
        platform: if cfg!(target_os = "macos") {
            capability_sets::MajorPlatformType::MACINTOSH
        } else {
            capability_sets::MajorPlatformType::UNIX
        },
        hardware_id: None,
        request_data: None,
        autologon: true,
        // Sound and microphones are not redirected; see `unsupported`.
        enable_audio_playback: false,
        enable_audio_capture: false,
        performance_flags: Default::default(),
        license_cache: None,
        timezone_info: Default::default(),
        compression_type: None,
        // The remote pointer is drawn into the picture, as egui cannot show custom cursors.
        enable_server_pointer: true,
        pointer_software_rendering: true,
        multitransport_flags: None,
        support_dyn_vc_gfx_protocol: false,
    })
}

/// Runs a connected session until it ends, the UI closes it, or the network drops.
async fn session(
    result: ConnectionResult,
    framed: Framed,
    settings: &mut Settings,
    commands: &mut UnboundedReceiver<Command>,
    clipboard: &mut UnboundedReceiver<ClipboardMessage>,
    offered: &Arc<Mutex<Option<String>>>,
    link: &Link,
) -> Stop {
    let (mut reader, mut writer) = split_tokio_framed(framed);
    let size = result.desktop_size;
    let mut image = DecodedImage::new(PixelFormat::RgbA32, size.width, size.height);
    if let Ok(mut frame) = link.frame.lock() {
        frame.resize(size.width.into(), size.height.into());
    }
    let activation = result.activation_factory;
    let mut stage = ActiveStageBuilder {
        static_channels: result.static_channels,
        user_channel_id: result.user_channel_id,
        io_channel_id: result.io_channel_id,
        message_channel_id: result.message_channel_id,
        share_id: result.share_id,
        compression_type: result.compression_type,
        enable_server_pointer: result.enable_server_pointer,
        pointer_software_rendering: result.pointer_software_rendering,
    }
    .build();
    let started = Instant::now();
    // A resize waits until the display control channel is ready.
    let mut pending: Option<([u16; 2], u32)> = None;
    let mut fixed_reported = false;
    let mut ticks = tokio::time::interval(Duration::from_secs(1));

    loop {
        let outputs = tokio::select! {
            frame = reader.read_pdu() => {
                let (action, payload) = match frame {
                    Ok(frame) => frame,
                    Err(e) => return Stop::Dropped(e.to_string()),
                };
                stage.process(&mut image, action, &payload, reader.last_read_at())
            }
            command = commands.recv() => match command {
                None => return Stop::Closed,
                Some(Command::Input(events)) => stage.process_fastpath_input(&mut image, &events),
                Some(Command::Resize(size, scale)) => {
                    settings.size = size;
                    settings.scale = scale;
                    pending = Some((size, scale));
                    Ok(vec![])
                }
                Some(Command::Offer(text)) => {
                    if let Ok(mut offered) = offered.lock() {
                        *offered = Some(text);
                    }
                    clipboard_frame(&mut stage, ClipboardMessage::SendInitiateCopy(clipboard::text_formats()))
                }
            },
            Some(message) = clipboard.recv() => clipboard_frame(&mut stage, message),
            _ = ticks.tick() => {
                // Lets the clipboard expire stale locks.
                match stage.get_svc_processor_mut::<CliprdrClient>().map(|c| c.drive_timeouts()) {
                    Some(Ok(messages)) => stage
                        .process_svc_processor_messages(messages)
                        .map(|f| if f.is_empty() { vec![] } else { vec![ActiveStageOutput::ResponseFrame(f)] }),
                    _ => Ok(vec![]),
                }
            }
        };
        let outputs = match outputs {
            Ok(outputs) => outputs,
            Err(e) => return Stop::Dropped(e.report().to_string()),
        };
        if let Some((size, scale)) = pending {
            let (width, height) =
                MonitorLayoutEntry::adjust_display_size(size[0].into(), size[1].into());
            match stage.encode_resize(width, height, Some(scale), None) {
                Some(Ok(frame)) => {
                    pending = None;
                    if let Err(e) = writer.write_all(&frame).await {
                        return Stop::Dropped(e.to_string());
                    }
                }
                Some(Err(e)) => return Stop::Dropped(e.report().to_string()),
                None if started.elapsed() > Duration::from_secs(10) && !fixed_reported => {
                    fixed_reported = true;
                    link.send(Event::FixedResolution);
                }
                None => {}
            }
        }
        for output in outputs {
            match output {
                ActiveStageOutput::ResponseFrame(frame) => {
                    if let Err(e) = writer.write_all(&frame).await {
                        return Stop::Dropped(e.to_string());
                    }
                }
                ActiveStageOutput::GraphicsUpdate(region) => {
                    if let Ok(mut frame) = link.frame.lock() {
                        frame.copy(
                            image.data(),
                            Region {
                                left: region.left.into(),
                                top: region.top.into(),
                                right: usize::from(region.right) + 1,
                                bottom: usize::from(region.bottom) + 1,
                            },
                        );
                    }
                    link.ctx.request_repaint();
                }
                ActiveStageOutput::Terminate(reason) => return Stop::Ended(reason.to_string()),
                ActiveStageOutput::DeactivateAll => {
                    // The server changes the resolution by reactivating the connection.
                    let mut sequence = activation.create();
                    let mut buf = WriteBuf::new();
                    let finalized = loop {
                        let written =
                            match single_sequence_step_read(&mut reader, &mut sequence, &mut buf)
                                .await
                            {
                                Ok(written) => written,
                                Err(e) => return Stop::Dropped(e.report().to_string()),
                            };
                        if written.size().is_some()
                            && let Err(e) = writer.write_all(buf.filled()).await
                        {
                            return Stop::Dropped(e.to_string());
                        }
                        if let ConnectionActivationState::Finalized {
                            desktop_size,
                            share_id,
                            static_channel_chunk_size,
                            enable_server_pointer,
                            pointer_software_rendering,
                            ..
                        } = sequence.connection_activation_state()
                        {
                            break (
                                desktop_size,
                                share_id,
                                static_channel_chunk_size,
                                enable_server_pointer,
                                pointer_software_rendering,
                            );
                        }
                    };
                    let (size, share_id, chunk_size, server_pointer, software_pointer) = finalized;
                    image = DecodedImage::new(PixelFormat::RgbA32, size.width, size.height);
                    if let Ok(mut frame) = link.frame.lock() {
                        frame.resize(size.width.into(), size.height.into());
                    }
                    link.ctx.request_repaint();
                    if !stage.reactivate(
                        sequence.io_channel_id(),
                        sequence.user_channel_id(),
                        share_id,
                        server_pointer,
                        software_pointer,
                        chunk_size,
                    ) {
                        return Stop::Dropped(
                            "the server reactivated with invalid settings".into(),
                        );
                    }
                }
                // Pointer shapes are drawn into the picture; other updates need no response.
                _ => {}
            }
        }
    }
}

/// Sends a clipboard backend message over the CLIPRDR channel.
fn clipboard_frame(
    stage: &mut ActiveStage,
    message: ClipboardMessage,
) -> ironrdp_session::SessionResult<Vec<ActiveStageOutput>> {
    let Some(cliprdr) = stage.get_svc_processor_mut::<CliprdrClient>() else {
        return Ok(vec![]);
    };
    let messages = match message {
        ClipboardMessage::SendInitiateCopy(formats) => cliprdr.initiate_copy(&formats),
        ClipboardMessage::SendFormatData(response) => cliprdr.submit_format_data(response),
        ClipboardMessage::SendInitiatePaste(format) => cliprdr.initiate_paste(format),
        // Files are not transferred.
        _ => return Ok(vec![]),
    };
    let messages = messages.map_err(|e| ironrdp_session::custom_err!("clipboard", e))?;
    let frame = stage.process_svc_processor_messages(messages)?;
    Ok(vec![ActiveStageOutput::ResponseFrame(frame)])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        Settings {
            host: "desk.example".into(),
            port: 3389,
            gateway: None,
            username: "alex".into(),
            domain: Some("CORP".into()),
            password: "secret".into(),
            size: [1600, 900],
            scale: 150,
            clipboard: true,
        }
    }

    #[test]
    fn settings_become_connector_config() {
        let config = config(&settings()).expect("config");
        assert_eq!(
            (config.desktop_size.width, config.desktop_size.height),
            (1600, 900)
        );
        assert_eq!(config.desktop_scale_factor, 150);
        assert!(config.enable_credssp && config.enable_tls);
        assert!(!config.enable_standard_rdp_security);
        assert_eq!(config.domain.as_deref(), Some("CORP"));
        assert!(matches!(
            config.credentials,
            Credentials::UsernamePassword { ref username, ref password }
                if username == "alex" && password == "secret"
        ));
        assert!(!config.enable_audio_playback && !config.enable_audio_capture);
        assert!(config.pointer_software_rendering);
        assert_eq!(qualified_user(&settings()), r"CORP\alex");
    }

    #[test]
    fn trusted_certificates_pass_validation() {
        let untrusted = Arc::new(Mutex::new(None));
        let verify = verifier(&untrusted);
        assert!(!verify(b"certificate", "trust.example", "UnknownIssuer"));
        let recorded = untrusted.lock().unwrap().take().expect("recorded");
        assert_eq!(recorded.host, "trust.example");
        assert_eq!(recorded.reason, "UnknownIssuer");
        assert_eq!(recorded.fingerprint_text().len(), 32 * 3 - 1);
        trust(&recorded);
        assert!(verify(b"certificate", "trust.example", "UnknownIssuer"));
        assert!(untrusted.lock().unwrap().is_none());
        // Trust is per host and per certificate.
        assert!(!verify(b"certificate", "other.example", "UnknownIssuer"));
        assert!(!verify(b"changed", "trust.example", "UnknownIssuer"));
    }

    #[test]
    fn failures_ask_for_what_can_fix_them() {
        let certificate = Untrusted {
            host: "desk.example".into(),
            fingerprint: [0; 32],
            reason: "NotValidForName".into(),
        };
        let error = ironrdp_connector::general_err!("TLS");
        assert_eq!(
            classify(&error, Some(certificate.clone()), "desk.example"),
            Outcome::Certificate(certificate)
        );
        let denied = ConnectorError::new("CredSSP", ConnectorErrorKind::AccessDenied);
        assert!(matches!(classify(&denied, None, "h"), Outcome::SignIn(_)));
        assert!(matches!(
            classify(&error, None, "desk.example"),
            Outcome::Failed(message) if message.starts_with("Cannot connect to desk.example")
        ));
    }
}
