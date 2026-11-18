//! Answers OpenSSH's prompts (passwords, passphrases, one-time codes, unknown host keys) in
//! Relay's window for ssh processes that run without a terminal.
//!
//! Relay names its own executable as `SSH_ASKPASS`. Started that way, it is a helper: it
//! connects back to the Relay window over a local IPC path, proves it was started for that
//! ssh process with a random secret and by being a child of that ssh, and prints the answer
//! for ssh. Answers never touch the disk.
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::{mpsc, oneshot, watch},
};

/// Where the helper finds the window: `secret path` (secret first so the path may contain spaces).
pub const ENV: &str = "RELAY_ASKPASS";

/// The longest request line a helper may send.
const MAX_REQUEST: u64 = 64 * 1024;

/// What ssh wants from the person at the window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    /// A password, passphrase, or code, typed without showing it.
    Secret,
    /// A yes/no question answered with text, such as accepting an unknown host key.
    YesNo,
    /// A yes/no question answered by the helper's exit status, such as agent confirmation.
    Confirm,
    /// Information with nothing to answer, such as "touch your security key". ssh ends the
    /// helper when it is done.
    Notice,
}

impl Kind {
    /// The kind of prompt, from ssh's `SSH_ASKPASS_PROMPT` hint and the prompt's text.
    pub fn of(hint: &str, text: &str) -> Self {
        match hint {
            "confirm" => Self::Confirm,
            "none" => Self::Notice,
            _ if text.contains("(yes/no") => Self::YesNo,
            _ => Self::Secret,
        }
    }
}

/// A question from ssh, waiting for an answer from the window.
pub struct Request {
    pub id: u64,
    pub text: String,
    pub kind: Kind,
    /// Sends the answer; `None` declines. Notices never need one.
    pub reply: oneshot::Sender<Option<String>>,
}

/// What the window hears from its helpers.
pub enum Message {
    Ask(Request),
    /// The helper for request `id` went away before it was answered, usually because ssh
    /// finished with it (notices) or ended.
    Gone(u64),
}

#[derive(Serialize, Deserialize)]
struct Question {
    secret: String,
    text: String,
    hint: String,
}

#[derive(Serialize, Deserialize)]
struct Answer {
    answer: Option<String>,
}

/// Runs Relay as ssh's askpass helper when ssh started it that way, returning its exit code.
pub fn helper() -> Option<i32> {
    let endpoint = std::env::var(ENV).ok()?;
    let text = std::env::args().nth(1).unwrap_or_default();
    let hint = std::env::var("SSH_ASKPASS_PROMPT").unwrap_or_default();
    Some(match ask(&endpoint, &text, &hint) {
        Ok(Some(answer)) => {
            use std::io::Write;
            let mut out = std::io::stdout().lock();
            match writeln!(out, "{answer}").and_then(|_| out.flush()) {
                Ok(()) => 0,
                Err(_) => 1,
            }
        }
        Ok(None) | Err(_) => 1,
    })
}

/// Asks the window at `endpoint` and waits for its answer.
fn ask(endpoint: &str, text: &str, hint: &str) -> Result<Option<String>, String> {
    let (secret, path) = parse_endpoint(endpoint)?;
    let question = Question {
        secret: secret.into(),
        text: text.into(),
        hint: hint.into(),
    };
    let mut line = serde_json::to_vec(&question).map_err(|e| e.to_string())?;
    line.push(b'\n');
    ipc::ask(path, &line)
}

fn parse_endpoint(endpoint: &str) -> Result<(&str, &str), String> {
    let (secret, path) = endpoint.split_once(' ').ok_or("malformed endpoint")?;
    if secret.len() != 64 || !secret.bytes().all(|b| b.is_ascii_hexdigit()) || path.is_empty() {
        return Err("malformed endpoint".into());
    }
    Ok((secret, path))
}

fn new_secret() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| format!("Cannot prepare password prompts: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// A local listener for one ssh process's helpers.
pub struct Listener {
    transport: ipc::Transport,
    secret: String,
}

impl Listener {
    pub async fn bind() -> Result<Self, String> {
        Ok(Self {
            transport: ipc::Transport::bind().await?,
            secret: new_secret()?,
        })
    }

    /// The value of [`ENV`] for the ssh process.
    pub fn endpoint(&self) -> String {
        format!("{} {}", self.secret, self.transport.path())
    }

    /// Accepts helpers until `messages` closes. `ssh_pid` must receive the ssh child id before
    /// a helper is answered; only processes whose parent is that ssh are accepted.
    pub async fn serve(
        self,
        messages: mpsc::UnboundedSender<Message>,
        mut ssh_pid: watch::Receiver<Option<u32>>,
    ) {
        let mut next = 0u64;
        let secret = self.secret;
        self.transport
            .serve(&messages, &mut ssh_pid, &mut next, &secret)
            .await;
    }
}

/// Serves one helper: checks parent pid and secret, passes the question on, returns the answer.
async fn answer<R, W>(
    read: R,
    mut write: W,
    id: u64,
    secret: String,
    messages: mpsc::UnboundedSender<Message>,
) where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut read = tokio::io::BufReader::new(read.take(MAX_REQUEST));
    let mut line = String::new();
    let request = tokio::time::timeout(Duration::from_secs(10), read.read_line(&mut line));
    if !matches!(request.await, Ok(Ok(n)) if n > 0) {
        return;
    }
    let Ok(question) = serde_json::from_str::<Question>(&line) else {
        return;
    };
    if !same(question.secret.as_bytes(), secret.as_bytes()) {
        return;
    }
    let (reply, answered) = oneshot::channel();
    let kind = Kind::of(&question.hint, &question.text);
    let text = question.text.trim_end().to_string();
    if messages
        .send(Message::Ask(Request {
            id,
            text,
            kind,
            reply,
        }))
        .is_err()
    {
        return;
    }
    // The helper sends nothing more, so a finished read means it went away.
    let mut rest = [0u8; 1];
    let answer = tokio::select! {
        answer = answered => answer.ok().flatten(),
        _ = read.read(&mut rest) => {
            let _ = messages.send(Message::Gone(id));
            return;
        }
    };
    let mut reply = serde_json::to_vec(&Answer { answer }).unwrap_or_default();
    reply.push(b'\n');
    let _ = write.write_all(&reply).await;
    let _ = write.shutdown().await;
}

/// True when `peer` was started by `ssh` (direct child), so a stolen endpoint cannot be used
/// from an unrelated same-user process.
fn peer_from_ssh(peer: u32, ssh: u32) -> bool {
    parent_pid(peer) == Some(ssh)
}

/// Compares secrets in time that does not depend on where they differ.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(windows)]
fn parent_pid(pid: u32) -> Option<u32> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap.is_null() || snap == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
            return None;
        }
        let mut entry = std::mem::zeroed::<PROCESSENTRY32W>();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut parent = None;
        if Process32FirstW(snap, &mut entry) != 0 {
            loop {
                if entry.th32ProcessID == pid {
                    parent = Some(entry.th32ParentProcessID);
                    break;
                }
                if Process32NextW(snap, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
        parent
    }
}

#[cfg(target_os = "linux")]
fn parent_pid(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let close = stat.rfind(')')?;
    let mut fields = stat[close + 1..].split_whitespace();
    let _state = fields.next()?;
    fields.next()?.parse().ok()
}

#[cfg(target_os = "macos")]
fn parent_pid(pid: u32) -> Option<u32> {
    // `proc_bsdshortinfo`: pbsi_pid at 0, pbsi_ppid at 4. Read only those fields so a
    // mismatched full struct layout cannot break askpass on a Darwin update.
    const PROC_PIDT_SHORTBSDINFO: i32 = 13;
    unsafe extern "C" {
        fn proc_pidinfo(
            pid: i32,
            flavor: i32,
            arg: u64,
            buffer: *mut libc::c_void,
            buffersize: i32,
        ) -> i32;
    }
    unsafe {
        let mut buf = [0u8; 128];
        let wrote = proc_pidinfo(
            pid as i32,
            PROC_PIDT_SHORTBSDINFO,
            0,
            buf.as_mut_ptr() as *mut libc::c_void,
            buf.len() as i32,
        );
        if wrote < 8 {
            return None;
        }
        let ppid = u32::from_ne_bytes(buf[4..8].try_into().ok()?);
        (ppid > 0).then_some(ppid)
    }
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn parent_pid(_pid: u32) -> Option<u32> {
    None
}

#[cfg(unix)]
mod ipc {
    use super::*;
    use std::path::{Path, PathBuf};
    use tokio::net::{UnixListener, UnixStream};

    pub struct Transport {
        listener: UnixListener,
        path: PathBuf,
    }

    impl Transport {
        pub async fn bind() -> Result<Self, String> {
            let mut bytes = [0u8; 8];
            getrandom::fill(&mut bytes)
                .map_err(|e| format!("Cannot prepare password prompts: {e}"))?;
            let name = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
            let path = std::env::temp_dir().join(format!("relay-askpass-{name}.sock"));
            let _ = std::fs::remove_file(&path);
            let listener = UnixListener::bind(&path)
                .map_err(|e| format!("Cannot prepare password prompts: {e}"))?;
            // Same-user access is gated by the ssh parent check; block other accounts.
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
            Ok(Self { listener, path })
        }

        pub fn path(&self) -> String {
            self.path.to_string_lossy().into_owned()
        }

        pub async fn serve(
            self,
            messages: &mpsc::UnboundedSender<Message>,
            ssh_pid: &mut watch::Receiver<Option<u32>>,
            next: &mut u64,
            secret: &str,
        ) {
            loop {
                let accepted = tokio::select! {
                    accepted = self.listener.accept() => accepted,
                    _ = messages.closed() => break,
                };
                let Ok((stream, _)) = accepted else {
                    continue;
                };
                if ssh_pid.borrow().is_none()
                    && ssh_pid.wait_for(|pid| pid.is_some()).await.is_err()
                {
                    break;
                }
                let Some(ssh) = *ssh_pid.borrow() else {
                    break;
                };
                let Some(peer) = peer_pid(&stream) else {
                    continue;
                };
                if !peer_from_ssh(peer, ssh) {
                    continue;
                }
                *next += 1;
                let (read, write) = stream.into_split();
                tokio::spawn(answer(
                    read,
                    write,
                    *next,
                    secret.to_string(),
                    messages.clone(),
                ));
            }
            let _ = std::fs::remove_file(&self.path);
        }
    }

    pub fn ask(path: &str, line: &[u8]) -> Result<Option<String>, String> {
        let stream =
            std::os::unix::net::UnixStream::connect(Path::new(path)).map_err(|e| e.to_string())?;
        stream.set_write_timeout(Some(Duration::from_secs(5))).ok();
        use std::io::{BufRead, BufReader, Write};
        let mut stream = stream;
        stream.write_all(line).map_err(|e| e.to_string())?;
        let mut reply = String::new();
        BufReader::new(stream)
            .read_line(&mut reply)
            .map_err(|e| e.to_string())?;
        if reply.is_empty() {
            return Ok(None);
        }
        let reply: Answer = serde_json::from_str(&reply).map_err(|e| e.to_string())?;
        Ok(reply.answer.filter(|a| !a.contains(['\n', '\r'])))
    }

    #[cfg(target_os = "linux")]
    fn peer_pid(stream: &UnixStream) -> Option<u32> {
        u32::try_from(stream.peer_cred().ok()?.pid()?).ok()
    }

    #[cfg(target_os = "macos")]
    fn peer_pid(stream: &UnixStream) -> Option<u32> {
        use std::os::fd::AsRawFd;
        // LOCAL_PEERPID
        const SOL_LOCAL: i32 = 0;
        const LOCAL_PEERPID: i32 = 0x002;
        unsafe {
            let mut pid: libc::pid_t = 0;
            let mut len = std::mem::size_of_val(&pid) as libc::socklen_t;
            let rc = libc::getsockopt(
                stream.as_raw_fd(),
                SOL_LOCAL,
                LOCAL_PEERPID,
                &mut pid as *mut _ as *mut libc::c_void,
                &mut len,
            );
            (rc == 0 && pid > 0).then_some(pid as u32)
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn peer_pid(_stream: &UnixStream) -> Option<u32> {
        None
    }
}

#[cfg(windows)]
mod ipc {
    use super::*;
    use std::os::windows::io::AsRawHandle;
    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions};

    pub struct Transport {
        name: String,
        /// Created in [`Self::bind`] so helpers can connect as soon as the endpoint is published.
        first: Option<NamedPipeServer>,
    }

    impl Transport {
        pub async fn bind() -> Result<Self, String> {
            let mut bytes = [0u8; 8];
            getrandom::fill(&mut bytes)
                .map_err(|e| format!("Cannot prepare password prompts: {e}"))?;
            let name = format!(
                r"\\.\pipe\relay-askpass-{}",
                bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
            );
            let first = ServerOptions::new()
                .first_pipe_instance(true)
                .reject_remote_clients(true)
                .create(&name)
                .map_err(|e| format!("Cannot prepare password prompts: {e}"))?;
            Ok(Self {
                name,
                first: Some(first),
            })
        }

        pub fn path(&self) -> String {
            self.name.clone()
        }

        pub async fn serve(
            mut self,
            messages: &mpsc::UnboundedSender<Message>,
            ssh_pid: &mut watch::Receiver<Option<u32>>,
            next: &mut u64,
            secret: &str,
        ) {
            loop {
                if messages.is_closed() {
                    break;
                }
                let server = match self.first.take() {
                    Some(server) => server,
                    None => match ServerOptions::new()
                        .first_pipe_instance(false)
                        .reject_remote_clients(true)
                        .create(&self.name)
                    {
                        Ok(server) => server,
                        Err(_) => break,
                    },
                };
                let connected = tokio::select! {
                    result = server.connect() => result,
                    _ = messages.closed() => break,
                };
                if connected.is_err() {
                    continue;
                }
                if ssh_pid.borrow().is_none()
                    && ssh_pid.wait_for(|pid| pid.is_some()).await.is_err()
                {
                    break;
                }
                let Some(ssh) = *ssh_pid.borrow() else {
                    break;
                };
                let Some(peer) = peer_pid(&server) else {
                    continue;
                };
                if !peer_from_ssh(peer, ssh) {
                    continue;
                }
                *next += 1;
                let (read, write) = tokio::io::split(server);
                tokio::spawn(answer(
                    read,
                    write,
                    *next,
                    secret.to_string(),
                    messages.clone(),
                ));
            }
        }
    }

    pub fn ask(path: &str, line: &[u8]) -> Result<Option<String>, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        runtime.block_on(async {
            let mut client = ClientOptions::new().open(path).map_err(|e| e.to_string())?;
            client.write_all(line).await.map_err(|e| e.to_string())?;
            let mut read = tokio::io::BufReader::new(client);
            let mut reply = String::new();
            read.read_line(&mut reply)
                .await
                .map_err(|e| e.to_string())?;
            if reply.is_empty() {
                return Ok(None);
            }
            let reply: Answer = serde_json::from_str(&reply).map_err(|e| e.to_string())?;
            Ok(reply.answer.filter(|a| !a.contains(['\n', '\r'])))
        })
    }

    fn peer_pid(server: &tokio::net::windows::named_pipe::NamedPipeServer) -> Option<u32> {
        use windows_sys::Win32::System::Pipes::GetNamedPipeClientProcessId;
        unsafe {
            let mut pid = 0u32;
            if GetNamedPipeClientProcessId(server.as_raw_handle() as _, &mut pid) == 0 {
                None
            } else {
                Some(pid)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompts_are_recognized() {
        let host = "The authenticity of host 'build (192.0.2.1)' can't be established.\n\
                    ED25519 key fingerprint is SHA256:abc.\n\
                    Are you sure you want to continue connecting (yes/no/[fingerprint])? ";
        assert_eq!(Kind::of("", host), Kind::YesNo);
        assert_eq!(Kind::of("", "deploy@build's password: "), Kind::Secret);
        assert_eq!(Kind::of("confirm", "Allow use of key?"), Kind::Confirm);
        assert_eq!(Kind::of("none", "Confirm user presence"), Kind::Notice);
        assert!(same(b"abc", b"abc") && !same(b"abc", b"abd") && !same(b"abc", b"ab"));
    }

    /// Runs a listener on its own thread, answering each question with `answer`.
    /// `ssh_pid` is the process id helpers must be children of; `None` uses this process's parent
    /// so in-process [`ask`] calls are accepted.
    fn window(
        answer: Option<&'static str>,
        ssh_pid: Option<u32>,
        delay: Duration,
    ) -> (String, std::sync::mpsc::Receiver<String>) {
        let (seen, questions) = std::sync::mpsc::channel();
        let (endpoint, ready) = std::sync::mpsc::channel();
        let expected = ssh_pid.or_else(|| parent_pid(std::process::id()));
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let listener = Listener::bind().await.unwrap();
                endpoint.send(listener.endpoint()).unwrap();
                let (messages, mut incoming) = mpsc::unbounded_channel();
                let (pid_tx, pid_rx) = watch::channel(expected);
                tokio::spawn(listener.serve(messages, pid_rx));
                // Keep the sender alive for the serve task.
                let _pid_tx = pid_tx;
                while let Some(message) = incoming.recv().await {
                    if let Message::Ask(request) = message {
                        seen.send(request.text).unwrap();
                        tokio::time::sleep(delay).await;
                        let _ = request.reply.send(answer.map(String::from));
                    }
                }
            });
        });
        (ready.recv().unwrap(), questions)
    }

    #[test]
    fn helpers_wait_for_a_delayed_answer_from_the_window() {
        let (endpoint, questions) = window(Some("hunter2"), None, Duration::from_secs(6));
        assert_eq!(
            ask(&endpoint, "Password: ", "").unwrap().as_deref(),
            Some("hunter2")
        );
        assert_eq!(questions.recv().unwrap(), "Password:");
    }

    #[test]
    fn declining_and_wrong_secrets_answer_nothing() {
        let (endpoint, questions) = window(None, None, Duration::ZERO);
        assert_eq!(ask(&endpoint, "Password: ", "").unwrap(), None);
        assert_eq!(questions.recv().unwrap(), "Password:");
        let (_secret, path) = parse_endpoint(&endpoint).unwrap();
        let forged = format!("{} {path}", "0".repeat(64));
        assert_eq!(ask(&forged, "Password: ", "").unwrap(), None);
        assert!(
            questions.recv_timeout(Duration::from_millis(300)).is_err(),
            "a helper without the secret reached the window"
        );
        assert!(ask("not-a-secret /tmp/nope", "Password: ", "").is_err());
    }

    #[test]
    fn helpers_from_other_parents_are_rejected() {
        // A pid that is not the parent of this test process.
        let (endpoint, questions) = window(Some("hunter2"), Some(u32::MAX - 7), Duration::ZERO);
        // Closing an unread socket can give the rejected helper either EOF or a reset.
        assert!(!matches!(ask(&endpoint, "Password: ", ""), Ok(Some(_))));
        assert!(
            questions.recv_timeout(Duration::from_millis(500)).is_err(),
            "a helper whose parent is not ssh reached the window"
        );
    }
}
